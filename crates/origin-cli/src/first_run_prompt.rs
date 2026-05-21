//! First-run pending-prompt seed.
//!
//! `origin init`'s post-config walkthrough writes a markdown prompt to
//! `~/.origin/pending-prompt.txt`. The first time the TUI starts after
//! init, `main.rs` reads the file, fires it as the user's first prompt,
//! and deletes the file so it can never fire twice. The auto-fired prompt
//! asks the agent to discover and import skills from non-standard
//! locations — the LLM-driven discovery the operator wanted, deferred from
//! init time (when the daemon isn't yet running) to first-chat time
//! (when it is).

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Resolve `~/.origin/pending-prompt.txt`. Honors `$ORIGIN_HOME` for tests
/// and alternate-root installs, matching `crate::config::path`.
pub fn path() -> Result<PathBuf> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or_else(|| anyhow::anyhow!("home directory not found"))?;
    Ok(home.join(".origin").join("pending-prompt.txt"))
}

/// The markdown body of the discovery prompt. Public so tests + welcome
/// screens can render it for the user as a preview.
#[must_use]
pub fn discovery_prompt_body() -> &'static str {
    "Please do a one-time skill and tool discovery sweep for this origin install.\n\
     \n\
     1. Use Glob to find every `SKILL.md` under `~/.claude/`, `~/.config/opencode/`, \
        `~/.kilocode/`, `~/.config/kilocode/`, `~/.cursor/`, `~/.vscode/`, `~/Library/Application Support/`, \
        and `~/AppData/` that is NOT already under `~/.origin/skills/`.\n\
     2. For each match, Read the file. If the YAML frontmatter parses and the \
        skill is not already in `~/.origin/skills/` (compare by body hash), \
        copy it into a new directory under `~/.origin/skills/<skill-name>/SKILL.md`.\n\
     3. For each imported skill, check the `allowed-tools:` list. Any tool NOT \
        in the built-in Toolbox is likely an MCP-served tool — note it for the user \
        with a `keyring add` or `mcp.toml` snippet they can run.\n\
     4. Summarize: how many skills were imported per source directory, how many \
        duplicates were skipped, and which (if any) declared tools the Toolbox \
        does not provide.\n\
     \n\
     Do not modify or delete the source files. After the summary, this prompt \
     will not run again."
}

/// Write the seed prompt to `p`. Overwrites any existing file (re-running
/// `origin init` re-arms the discovery).
pub fn seed_to(p: &Path) -> Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(p, discovery_prompt_body())?;
    Ok(())
}

/// Read + delete the pending prompt at `p`. Returns `Ok(None)` when the
/// file does not exist (steady state after first run). The delete is
/// best-effort — read errors propagate, but a delete failure is logged
/// and ignored so a permission glitch doesn't block the chat.
pub fn drain(p: &Path) -> Result<Option<String>> {
    let body = match std::fs::read_to_string(p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(anyhow::Error::from(e)),
    };
    if let Err(e) = std::fs::remove_file(p) {
        tracing::warn!(error = %e, "failed to remove pending-prompt.txt");
    }
    Ok(Some(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_then_drain_returns_body_and_removes_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("pending-prompt.txt");
        seed_to(&p).expect("seed");
        assert!(p.exists());
        let body = drain(&p).expect("drain").expect("present");
        assert!(body.contains("skill and tool discovery"));
        assert!(!p.exists(), "drain must delete the file");
    }

    #[test]
    fn drain_missing_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("never-written.txt");
        assert!(drain(&p).expect("drain").is_none());
    }

    #[test]
    fn discovery_prompt_mentions_each_source_dir() {
        let body = discovery_prompt_body();
        for src in &[".claude", ".config/opencode", ".kilocode"] {
            assert!(body.contains(src), "discovery prompt missing {src}");
        }
    }
}
