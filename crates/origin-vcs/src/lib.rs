// SPDX-License-Identifier: Apache-2.0
//! Agent-native git safety layer for `origin`.
//!
//! `origin`'s agents edit the user's tree directly; one bad turn can clobber
//! work. This crate adds a *shadow* git history (cline / kilocode checkpoints,
//! aider git-as-undo, gemini `/rewind`) plus a lightweight lane / draft-patch
//! model (jcode) so a turn's output can be reviewed before it lands.
//!
//! Every git effect is routed through an injected [`GitRunner`], so the whole
//! crate is unit-tested offline with a recording mock — no subprocess, no repo,
//! no network.
//!
//! ```
//! use origin_vcs::{parse_checkpoints, LOG_FORMAT};
//!
//! // `LOG_FORMAT` is the exact `git log --format=...` argument that produces a
//! // stream `parse_checkpoints` understands.
//! assert!(LOG_FORMAT.starts_with("--format="));
//! assert!(parse_checkpoints("").is_empty());
//! ```

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Field separator emitted by `%x1f` in the checkpoint log format.
const FIELD_SEP: char = '\u{1f}';
/// Record separator emitted by `%x1e` in the checkpoint log format.
const RECORD_SEP: char = '\u{1e}';

/// The `git log --format=...` argument whose output [`parse_checkpoints`] reads.
///
/// Each commit is one record terminated by `\x1e`; within a record the fields
/// (hash, subject/label, body) are separated by `\x1f`. The body carries the
/// machine-readable `ms=<unix_ms> files=<n>` metadata that [`ShadowGit::snapshot`]
/// writes, so a round-trip preserves the checkpoint's timestamp and file count.
pub const LOG_FORMAT: &str = "--format=%H\x1f%s\x1f%b\x1e";

/// Error raised by the VCS safety layer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VcsError {
    /// A git invocation failed; the payload is a human-readable reason.
    #[error("git failed: {0}")]
    Git(String),
    /// The referenced checkpoint id does not exist in the shadow repo.
    #[error("checkpoint not found: {0}")]
    NotFound(String),
}

/// Runs a single git subcommand and returns its stdout.
///
/// Implementors own *all* process / filesystem side effects, which is what makes
/// the rest of the crate pure and testable. `args` is the argument vector passed
/// to `git` (the program name itself is implicit).
pub trait GitRunner {
    /// Run `git <args...>` and return captured stdout on success.
    ///
    /// # Errors
    ///
    /// Returns [`VcsError::Git`] when the subprocess cannot be spawned or exits
    /// non-zero.
    fn run(&self, args: &[&str]) -> Result<String, VcsError>;
}

/// A single recoverable point in the shadow history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Commit id in the shadow repo (a git object hash).
    pub id: String,
    /// Human label supplied at snapshot time (e.g. the turn summary).
    pub label: String,
    /// Wall-clock creation time, Unix epoch milliseconds.
    pub created_at_unix_ms: u64,
    /// Number of files changed in the staged set when the snapshot was taken.
    pub files_changed: u32,
}

/// How much of a checkpoint to restore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RestoreMode {
    /// Overwrite the entire working tree from the checkpoint without moving HEAD
    /// (gemini `/rewind` of files only).
    WorkingTree,
    /// Restore only the listed paths from the checkpoint.
    Files(Vec<String>),
    /// Hard-reset HEAD and the working tree to the checkpoint (full rewind).
    Full,
}

/// A named lane: a base ref plus the ordered draft patches proposed against it.
///
/// Lanes let an agent stage several turns' worth of changes (jcode's
/// lane / draft-patch model) before any of them touches the user's real branch.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Lane {
    /// Lane name (an arbitrary identifier).
    pub name: String,
    /// The ref this lane is based on (commit id or branch name).
    pub base: String,
    /// Draft patches proposed in this lane, oldest first.
    pub draft_patches: Vec<DraftPatch>,
}

impl Lane {
    /// Create an empty lane based on `base`.
    #[must_use]
    pub fn new(name: impl Into<String>, base: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            base: base.into(),
            draft_patches: Vec::new(),
        }
    }

    /// Append a draft patch to the lane.
    pub fn push_draft(&mut self, patch: DraftPatch) {
        self.draft_patches.push(patch);
    }

    /// Number of draft patches currently in the lane.
    #[must_use]
    pub fn draft_count(&self) -> usize {
        self.draft_patches.len()
    }
}

/// A proposed-but-not-applied change, with the provenance of the agent/turn that
/// produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftPatch {
    /// Stable identifier for the patch.
    pub id: String,
    /// One-line summary of what the patch does.
    pub summary: String,
    /// Which agent / turn produced the patch (for audit and attribution).
    pub provenance: String,
}

impl DraftPatch {
    /// Construct a draft patch.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        summary: impl Into<String>,
        provenance: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            summary: summary.into(),
            provenance: provenance.into(),
        }
    }
}

/// A shadow git repository layered over the user's working tree.
///
/// All operations target a *separate* git directory (`shadow_dir`) so checkpoints
/// never pollute the user's real `.git`. Construct one with [`ShadowGit::new`].
pub struct ShadowGit<'a> {
    runner: &'a dyn GitRunner,
    shadow_dir: String,
}

impl<'a> ShadowGit<'a> {
    /// Create a shadow-git handle that drives `runner` against `shadow_dir`.
    #[must_use]
    pub const fn new(runner: &'a dyn GitRunner, shadow_dir: String) -> Self {
        Self { runner, shadow_dir }
    }

    /// The shadow git directory this handle targets.
    #[must_use]
    pub fn shadow_dir(&self) -> &str {
        &self.shadow_dir
    }

    /// Run a git subcommand against the shadow git directory.
    fn git(&self, args: &[&str]) -> Result<String, VcsError> {
        let mut full: Vec<&str> = Vec::with_capacity(args.len() + 2);
        full.push("--git-dir");
        full.push(self.shadow_dir.as_str());
        full.extend_from_slice(args);
        self.runner.run(&full)
    }

    /// Verify a checkpoint id resolves to an object in the shadow repo.
    fn ensure_exists(&self, id: &str) -> Result<(), VcsError> {
        self.git(&["cat-file", "-e", id])
            .map(|_| ())
            .map_err(|_| VcsError::NotFound(id.to_string()))
    }

    /// Stage every change and record a new checkpoint labelled `label`.
    ///
    /// `now_ms` is the wall-clock time (Unix epoch milliseconds) stamped into the
    /// checkpoint; injecting it keeps the function deterministic for tests.
    ///
    /// # Errors
    ///
    /// Returns [`VcsError::Git`] if any underlying git command fails.
    pub fn snapshot(&self, label: &str, now_ms: u64) -> Result<Checkpoint, VcsError> {
        self.git(&["add", "-A"])?;
        let staged = self.git(&["diff", "--cached", "--name-only"])?;
        let files = staged.lines().filter(|l| !l.trim().is_empty()).count();
        let files_changed = u32::try_from(files).unwrap_or(u32::MAX);

        let body = format!("origin-checkpoint ms={now_ms} files={files_changed}");
        self.git(&[
            "commit",
            "--allow-empty",
            "--message",
            label,
            "--message",
            &body,
        ])?;
        let id = self.git(&["rev-parse", "HEAD"])?.trim().to_string();

        Ok(Checkpoint {
            id,
            label: label.to_string(),
            created_at_unix_ms: now_ms,
            files_changed,
        })
    }

    /// List all checkpoints, newest first.
    ///
    /// # Errors
    ///
    /// Returns [`VcsError::Git`] if the `git log` invocation fails.
    pub fn list(&self) -> Result<Vec<Checkpoint>, VcsError> {
        let out = self.git(&["log", LOG_FORMAT])?;
        Ok(parse_checkpoints(&out))
    }

    /// Restore the working tree (or selected files) from checkpoint `id`.
    ///
    /// # Errors
    ///
    /// Returns [`VcsError::NotFound`] if `id` does not exist, or
    /// [`VcsError::Git`] if the restoring git command fails.
    pub fn restore(&self, id: &str, mode: &RestoreMode) -> Result<(), VcsError> {
        self.ensure_exists(id)?;
        match mode {
            RestoreMode::WorkingTree => {
                self.git(&["checkout", id, "--", "."])?;
            }
            RestoreMode::Files(paths) => {
                let mut args: Vec<&str> = Vec::with_capacity(paths.len() + 3);
                args.push("checkout");
                args.push(id);
                args.push("--");
                args.extend(paths.iter().map(String::as_str));
                self.git(&args)?;
            }
            RestoreMode::Full => {
                self.git(&["reset", "--hard", id])?;
            }
        }
        Ok(())
    }

    /// Return the patch (`git show -p`) for checkpoint `id`.
    ///
    /// # Errors
    ///
    /// Returns [`VcsError::NotFound`] if `id` does not exist, or
    /// [`VcsError::Git`] if `git show` fails.
    pub fn diff(&self, id: &str) -> Result<String, VcsError> {
        self.ensure_exists(id)?;
        self.git(&["show", "--stat", "-p", id])
    }
}

/// An isolated git *worktree* helper, for running an agent's task in a checkout
/// that is physically separate from the user's main working tree.
///
/// A worktree (jcode/openclaude-style isolated lanes) lets the overnight runner
/// branch off, do destructive work, then tear the checkout down without ever
/// disturbing the user's files. The *source* repository is implicit: it is
/// whatever repo the injected [`GitRunner`] is rooted at (mirroring how a
/// process-backed runner roots at the workspace), so this type only carries a
/// runner reference and emits `git worktree …` subcommands through it.
///
/// Every effect is routed through [`GitRunner::run`], so the helper is unit
/// tested offline with a recording mock — no subprocess, no repo, no network.
pub struct Worktree<'a> {
    runner: &'a dyn GitRunner,
}

impl<'a> Worktree<'a> {
    /// Create a worktree helper that drives `runner` (rooted at the source repo).
    #[must_use]
    pub const fn new(runner: &'a dyn GitRunner) -> Self {
        Self { runner }
    }

    /// Add a new worktree at `path`, creating and checking out a *new* branch.
    ///
    /// Emits `worktree add --quiet <path> -b <branch>`.
    ///
    /// # Errors
    ///
    /// Returns [`VcsError::Git`] if the underlying git command fails.
    pub fn add(&self, path: &Path, branch: &str) -> Result<(), VcsError> {
        let path = path.to_string_lossy();
        self.runner
            .run(&["worktree", "add", "--quiet", path.as_ref(), "-b", branch])
            .map(|_| ())
    }

    /// Add a worktree at `path`, checking out an *existing* `branch`.
    ///
    /// Emits `worktree add --quiet <path> <branch>` (no `-b`, so git resolves an
    /// existing ref rather than creating one).
    ///
    /// # Errors
    ///
    /// Returns [`VcsError::Git`] if the underlying git command fails.
    pub fn add_existing(&self, path: &Path, branch: &str) -> Result<(), VcsError> {
        let path = path.to_string_lossy();
        self.runner
            .run(&["worktree", "add", "--quiet", path.as_ref(), branch])
            .map(|_| ())
    }

    /// Remove the worktree at `path`, optionally `--force`-ing past a dirty tree.
    ///
    /// Emits `worktree remove [--force] <path>`.
    ///
    /// # Errors
    ///
    /// Returns [`VcsError::Git`] if the underlying git command fails.
    pub fn remove(&self, path: &Path, force: bool) -> Result<(), VcsError> {
        let path = path.to_string_lossy();
        let mut args: Vec<&str> = Vec::with_capacity(4);
        args.push("worktree");
        args.push("remove");
        if force {
            args.push("--force");
        }
        args.push(path.as_ref());
        self.runner.run(&args).map(|_| ())
    }

    /// Prune administrative records for worktrees whose directories are gone.
    ///
    /// Emits `worktree prune`.
    ///
    /// # Errors
    ///
    /// Returns [`VcsError::Git`] if the underlying git command fails.
    pub fn prune(&self) -> Result<(), VcsError> {
        self.runner.run(&["worktree", "prune"]).map(|_| ())
    }

    /// List the registered worktrees, returning each worktree's path.
    ///
    /// Emits `worktree list --porcelain` and extracts the `worktree <path>`
    /// entries from the porcelain stream (one per registered worktree, the main
    /// one first).
    ///
    /// # Errors
    ///
    /// Returns [`VcsError::Git`] if the underlying git command fails.
    pub fn list(&self) -> Result<Vec<String>, VcsError> {
        let out = self.runner.run(&["worktree", "list", "--porcelain"])?;
        Ok(out
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(str::to_owned)
            .collect())
    }
}

/// Parse `git log` output produced with [`LOG_FORMAT`] into checkpoints.
///
/// Empty / whitespace-only input yields an empty vector. Records missing fields
/// are skipped leniently rather than erroring, so a partially-written log never
/// panics the caller.
#[must_use]
pub fn parse_checkpoints(git_log: &str) -> Vec<Checkpoint> {
    git_log
        .split(RECORD_SEP)
        .filter_map(parse_record)
        .collect()
}

/// Parse one `\x1e`-delimited record into a [`Checkpoint`], or `None` if blank.
fn parse_record(record: &str) -> Option<Checkpoint> {
    let record = record.trim();
    if record.is_empty() {
        return None;
    }
    let mut fields = record.split(FIELD_SEP);
    let id = fields.next()?.trim().to_string();
    if id.is_empty() {
        return None;
    }
    let label = fields.next().unwrap_or("").trim().to_string();
    let (created_at_unix_ms, files_changed) = parse_meta(fields.next().unwrap_or(""));
    Some(Checkpoint {
        id,
        label,
        created_at_unix_ms,
        files_changed,
    })
}

/// Extract `ms=<u64>` and `files=<u32>` from a checkpoint body; defaults to 0.
fn parse_meta(body: &str) -> (u64, u32) {
    let mut ms = 0_u64;
    let mut files = 0_u32;
    for token in body.split_whitespace() {
        if let Some(v) = token.strip_prefix("ms=") {
            ms = v.parse().unwrap_or(0);
        } else if let Some(v) = token.strip_prefix("files=") {
            files = v.parse().unwrap_or(0);
        }
    }
    (ms, files)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// Recording mock: logs every call and replies from a scripted queue,
    /// falling back to empty stdout once the queue drains.
    #[derive(Default)]
    struct MockGit {
        calls: RefCell<Vec<Vec<String>>>,
        replies: RefCell<VecDeque<Result<String, VcsError>>>,
    }

    impl MockGit {
        fn scripted(replies: Vec<Result<String, VcsError>>) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                replies: RefCell::new(replies.into_iter().collect()),
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.borrow().clone()
        }
    }

    impl GitRunner for MockGit {
        fn run(&self, args: &[&str]) -> Result<String, VcsError> {
            self.calls
                .borrow_mut()
                .push(args.iter().copied().map(str::to_owned).collect());
            self.replies
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Ok(String::new()))
        }
    }

    fn has_call_with(calls: &[Vec<String>], needle: &str) -> bool {
        calls.iter().any(|c| c.iter().any(|a| a == needle))
    }

    #[test]
    fn snapshot_issues_add_and_commit_and_returns_checkpoint() {
        let mock = MockGit::scripted(vec![
            Ok(String::new()),          // add -A
            Ok("a.rs\nb.rs\n".into()),  // diff --cached --name-only -> 2 files
            Ok(String::new()),          // commit
            Ok("deadbeef\n".into()),    // rev-parse HEAD
        ]);
        let sg = ShadowGit::new(&mock, ".origin/shadow.git".to_string());
        let cp = sg.snapshot("turn 7: refactor parser", 1_700_000_000_000).unwrap();

        assert_eq!(cp.id, "deadbeef");
        assert_eq!(cp.label, "turn 7: refactor parser");
        assert_eq!(cp.created_at_unix_ms, 1_700_000_000_000);
        assert_eq!(cp.files_changed, 2);

        let calls = mock.calls();
        // Every call must target the shadow git dir.
        assert!(calls.iter().all(|c| c.contains(&"--git-dir".to_string())));
        assert!(calls.iter().all(|c| c.contains(&".origin/shadow.git".to_string())));
        assert!(has_call_with(&calls, "add"));
        assert!(has_call_with(&calls, "commit"));
        // The commit carried our label.
        assert!(calls
            .iter()
            .any(|c| c.contains(&"commit".to_string())
                && c.contains(&"turn 7: refactor parser".to_string())));
    }

    #[test]
    fn list_parses_log_via_runner() {
        let log = format!(
            "abc123{FIELD_SEP}first{FIELD_SEP}origin-checkpoint ms=100 files=3{RECORD_SEP}def456{FIELD_SEP}second{FIELD_SEP}origin-checkpoint ms=200 files=1{RECORD_SEP}"
        );
        let mock = MockGit::scripted(vec![Ok(log)]);
        let sg = ShadowGit::new(&mock, "shadow".to_string());
        let cps = sg.list().unwrap();

        assert_eq!(cps.len(), 2);
        assert_eq!(cps[0].id, "abc123");
        assert_eq!(cps[0].label, "first");
        assert_eq!(cps[0].created_at_unix_ms, 100);
        assert_eq!(cps[0].files_changed, 3);
        assert_eq!(cps[1].id, "def456");
        assert_eq!(cps[1].files_changed, 1);

        // list must use the canonical format argument.
        assert!(has_call_with(&mock.calls(), LOG_FORMAT));
    }

    #[test]
    fn restore_working_tree_emits_checkout_dot() {
        let mock = MockGit::scripted(vec![Ok(String::new()), Ok(String::new())]);
        let sg = ShadowGit::new(&mock, "shadow".to_string());
        sg.restore("cafe", &RestoreMode::WorkingTree).unwrap();

        let calls = mock.calls();
        // First: existence check; second: the checkout.
        assert_eq!(
            calls[0],
            vec!["--git-dir", "shadow", "cat-file", "-e", "cafe"]
        );
        assert_eq!(
            calls[1],
            vec!["--git-dir", "shadow", "checkout", "cafe", "--", "."]
        );
    }

    #[test]
    fn restore_files_emits_pathspec() {
        let mock = MockGit::scripted(vec![Ok(String::new()), Ok(String::new())]);
        let sg = ShadowGit::new(&mock, "shadow".to_string());
        let mode = RestoreMode::Files(vec!["src/a.rs".into(), "src/b.rs".into()]);
        sg.restore("cafe", &mode).unwrap();

        let calls = mock.calls();
        assert_eq!(
            calls[1],
            vec![
                "--git-dir", "shadow", "checkout", "cafe", "--", "src/a.rs", "src/b.rs"
            ]
        );
    }

    #[test]
    fn restore_full_emits_hard_reset() {
        let mock = MockGit::scripted(vec![Ok(String::new()), Ok(String::new())]);
        let sg = ShadowGit::new(&mock, "shadow".to_string());
        sg.restore("cafe", &RestoreMode::Full).unwrap();

        let calls = mock.calls();
        assert_eq!(
            calls[1],
            vec!["--git-dir", "shadow", "reset", "--hard", "cafe"]
        );
    }

    #[test]
    fn restore_missing_id_is_not_found() {
        // cat-file -e fails -> mapped to NotFound, and no checkout is attempted.
        let mock = MockGit::scripted(vec![Err(VcsError::Git("missing object".into()))]);
        let sg = ShadowGit::new(&mock, "shadow".to_string());
        let err = sg
            .restore("ghost", &RestoreMode::WorkingTree)
            .unwrap_err();
        assert_eq!(err, VcsError::NotFound("ghost".to_string()));
        assert_eq!(mock.calls().len(), 1, "must not run checkout after a miss");
    }

    #[test]
    fn diff_checks_existence_then_shows() {
        let mock = MockGit::scripted(vec![Ok(String::new()), Ok("@@ -1 +1 @@\n+x\n".into())]);
        let sg = ShadowGit::new(&mock, "shadow".to_string());
        let out = sg.diff("cafe").unwrap();
        assert!(out.contains("+x"));

        let calls = mock.calls();
        assert_eq!(calls[0][2], "cat-file");
        assert_eq!(
            calls[1],
            vec!["--git-dir", "shadow", "show", "--stat", "-p", "cafe"]
        );
    }

    #[test]
    fn parse_checkpoints_empty_and_multiline() {
        assert!(parse_checkpoints("").is_empty());
        assert!(parse_checkpoints("   \n  ").is_empty());

        let single =
            format!("h1{FIELD_SEP}only{FIELD_SEP}origin-checkpoint ms=42 files=9{RECORD_SEP}");
        let cps = parse_checkpoints(&single);
        assert_eq!(cps.len(), 1);
        assert_eq!(cps[0].created_at_unix_ms, 42);
        assert_eq!(cps[0].files_changed, 9);

        // A record missing its metadata body still parses with zeroed meta.
        let bare = format!("h2{FIELD_SEP}no-body{RECORD_SEP}");
        let cps = parse_checkpoints(&bare);
        assert_eq!(cps.len(), 1);
        assert_eq!(cps[0].label, "no-body");
        assert_eq!(cps[0].created_at_unix_ms, 0);
        assert_eq!(cps[0].files_changed, 0);
    }

    #[test]
    fn lane_and_draft_patch_model() {
        let mut lane = Lane::new("agent-lane", "main");
        assert_eq!(lane.draft_count(), 0);
        lane.push_draft(DraftPatch::new("p1", "add cache", "agent:planner/turn3"));
        lane.push_draft(DraftPatch::new("p2", "fix lint", "agent:fixer/turn4"));
        assert_eq!(lane.draft_count(), 2);
        assert_eq!(lane.base, "main");
        assert_eq!(lane.draft_patches[0].provenance, "agent:planner/turn3");

        // Lane round-trips through serde untouched.
        let json = serde_json::to_string(&lane).unwrap();
        let back: Lane = serde_json::from_str(&json).unwrap();
        assert_eq!(lane, back);
    }

    #[test]
    fn worktree_add_emits_quiet_add_with_new_branch() {
        let mock = MockGit::scripted(vec![Ok(String::new())]);
        let wt = Worktree::new(&mock);
        let path = Path::new("/tmp/wt-feat");
        wt.add(path, "feat/x").unwrap();

        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            vec![
                "worktree",
                "add",
                "--quiet",
                path.to_string_lossy().as_ref(),
                "-b",
                "feat/x"
            ]
        );
    }

    #[test]
    fn worktree_add_existing_emits_quiet_add_with_existing_branch() {
        let mock = MockGit::scripted(vec![Ok(String::new())]);
        let wt = Worktree::new(&mock);
        let path = Path::new("/tmp/wt-existing");
        wt.add_existing(path, "feat/y").unwrap();

        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            vec![
                "worktree",
                "add",
                "--quiet",
                path.to_string_lossy().as_ref(),
                "feat/y"
            ]
        );
    }

    #[test]
    fn worktree_remove_without_force_emits_plain_remove() {
        let mock = MockGit::scripted(vec![Ok(String::new())]);
        let wt = Worktree::new(&mock);
        let path = Path::new("/tmp/wt-gone");
        wt.remove(path, false).unwrap();

        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            vec!["worktree", "remove", path.to_string_lossy().as_ref()]
        );
    }

    #[test]
    fn worktree_remove_with_force_inserts_force_flag() {
        let mock = MockGit::scripted(vec![Ok(String::new())]);
        let wt = Worktree::new(&mock);
        let path = Path::new("/tmp/wt-gone");
        wt.remove(path, true).unwrap();

        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            vec![
                "worktree",
                "remove",
                "--force",
                path.to_string_lossy().as_ref()
            ]
        );
    }

    #[test]
    fn worktree_prune_emits_prune() {
        let mock = MockGit::scripted(vec![Ok(String::new())]);
        let wt = Worktree::new(&mock);
        wt.prune().unwrap();

        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], vec!["worktree", "prune"]);
    }

    #[test]
    fn worktree_list_emits_porcelain_and_parses_paths() {
        let porcelain = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\nworktree /tmp/wt-feat\nHEAD def\nbranch refs/heads/feat/x\n";
        let mock = MockGit::scripted(vec![Ok(porcelain.to_string())]);
        let wt = Worktree::new(&mock);
        let paths = wt.list().unwrap();

        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], vec!["worktree", "list", "--porcelain"]);
        assert_eq!(paths, vec!["/repo".to_string(), "/tmp/wt-feat".to_string()]);
    }

    #[test]
    fn worktree_add_maps_failure_to_git_error() {
        let mock = MockGit::scripted(vec![Err(VcsError::Git("boom".into()))]);
        let wt = Worktree::new(&mock);
        let path = Path::new("/tmp/wt-feat");
        let err = wt.add(path, "feat/x").unwrap_err();
        assert!(matches!(err, VcsError::Git(_)));
    }

    #[test]
    fn snapshot_with_no_staged_changes_reports_zero_files() {
        let mock = MockGit::scripted(vec![
            Ok(String::new()), // add
            Ok(String::new()), // diff --cached --name-only -> nothing
            Ok(String::new()), // commit (allowed empty)
            Ok("0000\n".into()),
        ]);
        let sg = ShadowGit::new(&mock, "shadow".to_string());
        let cp = sg.snapshot("empty", 5).unwrap();
        assert_eq!(cp.files_changed, 0);
        assert_eq!(cp.id, "0000");
        // --allow-empty must be present so empty turns still checkpoint.
        assert!(has_call_with(&mock.calls(), "--allow-empty"));
    }
}
