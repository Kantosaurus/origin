// SPDX-License-Identifier: Apache-2.0
//! `origin checkpoint` / `checkpoints` / `rewind` / `checkpoint-diff` — an
//! agent-native git safety layer backed by [`origin_vcs`].
//!
//! Every turn's working tree can be snapshotted into a *shadow* git directory
//! (`<cwd>/.origin/shadow.git`) that never pollutes the user's real `.git`
//! (cline / kilocode checkpoints, aider git-as-undo, gemini `/rewind`). All git
//! effects route through a [`CmdGit`] runner that shells out to `git` via
//! [`std::process::Command`]; the pure checkpoint logic lives in [`origin_vcs`].

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use origin_vcs::{GitRunner, RestoreMode, ShadowGit, VcsError};

/// A [`GitRunner`] that drives the system `git` binary.
struct CmdGit;

impl GitRunner for CmdGit {
    fn run(&self, args: &[&str]) -> Result<String, VcsError> {
        let output = Command::new("git")
            .args(args)
            .output()
            .map_err(|e| VcsError::Git(format!("spawning git: {e}")))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            Err(VcsError::Git(stderr))
        }
    }
}

/// Returns the absolute path to the shadow git directory under the current dir.
fn shadow_dir() -> Result<PathBuf> {
    let cwd = std::env::current_dir().map_err(|e| anyhow::anyhow!("resolving cwd: {e}"))?;
    Ok(cwd.join(".origin").join("shadow.git"))
}

/// Ensures the shadow repo exists, initializing it on first use.
///
/// Runs `git --git-dir <shadow> --work-tree <cwd> init` when `<shadow>` is
/// missing and pins `core.worktree` to the current directory so checkpoints
/// stage and restore against the user's real tree.
fn ensure_shadow(runner: &CmdGit, shadow: &Path, cwd: &Path) -> Result<()> {
    if shadow.exists() {
        return Ok(());
    }
    if let Some(parent) = shadow.parent() {
        std::fs::create_dir_all(parent).map_err(|e| anyhow::anyhow!("creating {}: {e}", parent.display()))?;
    }
    let shadow_s = shadow.to_string_lossy();
    let cwd_s = cwd.to_string_lossy();
    runner
        .run(&["--git-dir", &shadow_s, "--work-tree", &cwd_s, "init"])
        .map_err(|e| anyhow::anyhow!("initializing shadow repo: {e}"))?;
    runner
        .run(&["--git-dir", &shadow_s, "config", "core.worktree", &cwd_s])
        .map_err(|e| anyhow::anyhow!("setting shadow core.worktree: {e}"))?;
    Ok(())
}

/// Current wall-clock time as Unix epoch milliseconds (saturating).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Create a checkpoint of the current working tree.
///
/// # Errors
/// Returns on filesystem failure or when the underlying `git` invocation fails.
pub fn checkpoint(label: Option<String>) -> Result<()> {
    let runner = CmdGit;
    let shadow = shadow_dir()?;
    let cwd = std::env::current_dir().map_err(|e| anyhow::anyhow!("resolving cwd: {e}"))?;
    ensure_shadow(&runner, &shadow, &cwd)?;

    let sg = ShadowGit::new(&runner, shadow.to_string_lossy().into_owned());
    let label = label.unwrap_or_else(|| "checkpoint".to_owned());
    let cp = sg
        .snapshot(&label, now_ms())
        .map_err(|e| anyhow::anyhow!("creating checkpoint: {e}"))?;
    println!(
        "checkpoint {} created: {} ({} files changed)",
        cp.id, cp.label, cp.files_changed
    );
    Ok(())
}

/// List all checkpoints, newest first.
///
/// # Errors
/// Returns on filesystem failure or when the underlying `git log` fails.
pub fn checkpoints() -> Result<()> {
    let runner = CmdGit;
    let shadow = shadow_dir()?;
    if !shadow.exists() {
        println!("no checkpoints yet");
        return Ok(());
    }
    let sg = ShadowGit::new(&runner, shadow.to_string_lossy().into_owned());
    let list = sg
        .list()
        .map_err(|e| anyhow::anyhow!("listing checkpoints: {e}"))?;
    if list.is_empty() {
        println!("no checkpoints yet");
        return Ok(());
    }
    for cp in list {
        println!("{}  ({} files)  {}", cp.id, cp.files_changed, cp.label);
    }
    Ok(())
}

/// Restore the working tree from checkpoint `id`.
///
/// When `paths` is non-empty, only those paths are restored from the checkpoint
/// (a per-file selective revert, `RestoreMode::Files`) without moving HEAD.
/// Otherwise, with `files_only` only the tracked files are restored (gemini
/// `/rewind` of files only); without it HEAD and the working tree are hard-reset.
///
/// # Errors
/// Returns on filesystem failure, an unknown checkpoint, or a git failure.
pub fn rewind(id: &str, files_only: bool, paths: Vec<String>) -> Result<()> {
    let runner = CmdGit;
    let shadow = shadow_dir()?;
    if !shadow.exists() {
        anyhow::bail!("no checkpoints yet");
    }
    let sg = ShadowGit::new(&runner, shadow.to_string_lossy().into_owned());
    let path_count = paths.len();
    let mode = if path_count > 0 {
        RestoreMode::Files(paths)
    } else if files_only {
        RestoreMode::WorkingTree
    } else {
        RestoreMode::Full
    };
    match sg.restore(id, &mode) {
        Ok(()) => {
            let scope = if path_count > 0 {
                format!("{path_count} path(s)")
            } else if files_only {
                "working tree".to_string()
            } else {
                "HEAD + working tree".to_string()
            };
            println!("restored {scope} from checkpoint {id}");
            Ok(())
        }
        Err(VcsError::NotFound(_)) => {
            anyhow::bail!("no such checkpoint: {id}")
        }
        Err(e) => Err(anyhow::anyhow!("restoring checkpoint: {e}")),
    }
}

/// Value-returning checkpoint list for in-TUI consumers (gap 3 timeline).
///
/// Unlike [`checkpoints`] (which prints), this returns the checkpoints (newest
/// first) so the TUI can render them under the alternate screen. Returns an empty
/// list when no shadow repo exists yet.
///
/// # Errors
/// Returns when the underlying `git log` fails.
pub fn list_checkpoints() -> Result<Vec<origin_vcs::Checkpoint>> {
    let runner = CmdGit;
    let shadow = shadow_dir()?;
    if !shadow.exists() {
        return Ok(Vec::new());
    }
    let sg = ShadowGit::new(&runner, shadow.to_string_lossy().into_owned());
    sg.list().map_err(|e| anyhow::anyhow!("listing checkpoints: {e}"))
}

/// Value-returning patch for checkpoint `id` for the in-TUI diff viewer (gap 3).
///
/// # Errors
/// Returns when no shadow repo exists, the checkpoint is unknown, or git fails.
pub fn checkpoint_patch(id: &str) -> Result<String> {
    let runner = CmdGit;
    let shadow = shadow_dir()?;
    if !shadow.exists() {
        anyhow::bail!("no checkpoints yet");
    }
    let sg = ShadowGit::new(&runner, shadow.to_string_lossy().into_owned());
    match sg.diff(id) {
        Ok(patch) => Ok(patch),
        Err(VcsError::NotFound(_)) => anyhow::bail!("no such checkpoint: {id}"),
        Err(e) => Err(anyhow::anyhow!("diffing checkpoint: {e}")),
    }
}

/// Quiet restore for in-TUI revert (gap 3).
///
/// Restores from checkpoint `id` without printing (the caller renders the
/// outcome into the scrollback). `files_only` restores just the working tree
/// (HEAD unmoved); otherwise a full reset.
///
/// # Errors
/// Returns when no shadow repo exists, the checkpoint is unknown, or git fails.
pub fn rewind_to(id: &str, files_only: bool) -> Result<()> {
    let runner = CmdGit;
    let shadow = shadow_dir()?;
    if !shadow.exists() {
        anyhow::bail!("no checkpoints yet");
    }
    let sg = ShadowGit::new(&runner, shadow.to_string_lossy().into_owned());
    let mode = if files_only {
        RestoreMode::WorkingTree
    } else {
        RestoreMode::Full
    };
    match sg.restore(id, &mode) {
        Ok(()) => Ok(()),
        Err(VcsError::NotFound(_)) => anyhow::bail!("no such checkpoint: {id}"),
        Err(e) => Err(anyhow::anyhow!("restoring checkpoint: {e}")),
    }
}

/// Render checkpoints into display lines for the in-TUI `/timeline` (newest
/// first). Pure: the caller pushes these into the scrollable scrollback.
#[must_use]
pub fn format_checkpoint_lines(checkpoints: &[origin_vcs::Checkpoint]) -> Vec<String> {
    if checkpoints.is_empty() {
        return vec!["no checkpoints yet".to_string()];
    }
    let mut out = Vec::with_capacity(checkpoints.len() + 1);
    out.push(format!(
        "{} checkpoint(s), newest first — `/timeline <id>` views a diff, `/timeline revert <id>` restores:",
        checkpoints.len()
    ));
    for cp in checkpoints {
        out.push(format!("  {}  ({} files)  {}", cp.id, cp.files_changed, cp.label));
    }
    out
}

/// Print the patch for checkpoint `id`.
///
/// # Errors
/// Returns on filesystem failure, an unknown checkpoint, or a git failure.
pub fn checkpoint_diff(id: &str) -> Result<()> {
    let runner = CmdGit;
    let shadow = shadow_dir()?;
    if !shadow.exists() {
        anyhow::bail!("no checkpoints yet");
    }
    let sg = ShadowGit::new(&runner, shadow.to_string_lossy().into_owned());
    match sg.diff(id) {
        Ok(patch) => {
            print!("{patch}");
            Ok(())
        }
        Err(VcsError::NotFound(_)) => {
            anyhow::bail!("no such checkpoint: {id}")
        }
        Err(e) => Err(anyhow::anyhow!("diffing checkpoint: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::format_checkpoint_lines;
    use origin_vcs::Checkpoint;

    fn cp(id: &str, files: u32, label: &str) -> Checkpoint {
        Checkpoint {
            id: id.to_string(),
            label: label.to_string(),
            created_at_unix_ms: 0,
            files_changed: files,
        }
    }

    #[test]
    fn empty_list_renders_placeholder() {
        assert_eq!(
            format_checkpoint_lines(&[]),
            vec!["no checkpoints yet".to_string()]
        );
    }

    #[test]
    fn non_empty_list_has_header_and_one_line_per_checkpoint() {
        let cps = vec![cp("abc1234", 3, "turn"), cp("def5678", 1, "tool:Edit")];
        let lines = format_checkpoint_lines(&cps);
        assert_eq!(lines.len(), 3, "header + 2 checkpoints");
        assert!(lines[0].contains("2 checkpoint(s)"));
        assert!(lines[1].contains("abc1234") && lines[1].contains("3 files") && lines[1].contains("turn"));
        assert!(lines[2].contains("def5678") && lines[2].contains("tool:Edit"));
    }
}
