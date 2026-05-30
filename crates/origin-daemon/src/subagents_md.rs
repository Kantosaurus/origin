// SPDX-License-Identifier: Apache-2.0
//! Markdown-defined, tool-isolated subagents (gemini).
//!
//! Files under `~/.origin/subagents/*.md` with YAML frontmatter — `name`,
//! `description`, `allowed-tools` (the same schema as a `SKILL.md`, so we reuse
//! [`origin_skills::frontmatter::parse_frontmatter`]) — declare named sub-agents
//! the model can launch via the `Task` tool. The real swarm worker
//! ([`crate::swarm_worker`]) enforces each one's `allowed_tools`, so the isolation
//! is genuine, not advisory.
//!
//! The loaded set is surfaced to the model as an `<origin-subagents>` system
//! block. **Default-off:** with no `subagents/` dir (or no `.md` files) the block
//! is empty and the assembled system prompt — and its cache breakpoints — are
//! byte-identical to before.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// A declarative, tool-isolated subagent loaded from a `.md` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentDef {
    /// Subagent name (the `name` frontmatter field).
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Tools the subagent is allowed to use (the `allowed-tools` field).
    pub allowed_tools: Vec<String>,
}

/// `~/.origin/subagents`, honoring `ORIGIN_HOME` (tests) then the home dir.
fn subagents_dir() -> Option<PathBuf> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)?;
    Some(home.join(".origin").join("subagents"))
}

/// Load every `*.md` subagent definition from `dir`, skipping unreadable or
/// malformed files. Returns them sorted by name for a stable system block.
#[must_use]
pub fn load(dir: &Path) -> Vec<SubagentDef> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        match origin_skills::frontmatter::parse_frontmatter(&raw) {
            Ok(parsed) if !parsed.front.name.is_empty() => out.push(SubagentDef {
                name: parsed.front.name,
                description: parsed.front.description,
                allowed_tools: parsed.front.allowed_tools,
            }),
            Ok(_) => {}
            Err(e) => tracing::warn!(path = %path.display(), error = %e, "subagents: skipping malformed .md"),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Render the `<origin-subagents>` system block. An empty list yields an empty
/// string (byte-identical system prompt).
#[must_use]
pub fn catalog_block(defs: &[SubagentDef]) -> String {
    use std::fmt::Write as _;
    if defs.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "<origin-subagents>\n\
         Declarative sub-agents available IN THIS session. To run one, call the \
         `Task` tool with a goal for it and `allowed_tools` set to exactly the \
         tools listed for that sub-agent — it runs in isolation and cannot exceed \
         them. Each line is: name — [tools] — description.\n",
    );
    for d in defs {
        let tools = if d.allowed_tools.is_empty() {
            "(none)".to_string()
        } else {
            d.allowed_tools.join(",")
        };
        let _ = writeln!(out, "  - {} — [{}] — {}", d.name, tools, d.description);
    }
    out.push_str("</origin-subagents>");
    out
}

/// Process-wide cached `<origin-subagents>` block, built once on first use from
/// `~/.origin/subagents`. Empty (no dir / no files) ⇒ `""` ⇒ byte-identical.
#[must_use]
pub fn global_block() -> &'static str {
    static CELL: OnceLock<String> = OnceLock::new();
    CELL.get_or_init(|| subagents_dir().map_or_else(String::new, |d| catalog_block(&load(&d))))
        .as_str()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{catalog_block, load, SubagentDef};

    #[test]
    fn loads_and_renders_md_subagents() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("explorer.md"),
            "---\nname: explorer\ndescription: read-only researcher\nallowed-tools: [Read, Grep, Glob]\n---\nYou explore the codebase.\n",
        )
        .unwrap();
        // A non-md file and a malformed md are ignored.
        std::fs::write(dir.path().join("notes.txt"), "ignore me").unwrap();
        std::fs::write(dir.path().join("broken.md"), "no frontmatter here").unwrap();

        let defs = load(dir.path());
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "explorer");
        assert_eq!(defs[0].allowed_tools, vec!["Read", "Grep", "Glob"]);

        let block = catalog_block(&defs);
        assert!(block.contains("<origin-subagents>"));
        assert!(block.contains("explorer"));
        assert!(block.contains("Read,Grep,Glob"));
    }

    #[test]
    fn empty_list_is_empty_block() {
        assert!(catalog_block(&[]).is_empty());
    }

    #[test]
    fn missing_dir_loads_nothing() {
        let defs = load(std::path::Path::new("/no/such/subagents-dir-xyz"));
        assert!(defs.is_empty());
    }

    #[test]
    fn block_lists_tools_or_none() {
        let defs = vec![SubagentDef {
            name: "planner".into(),
            description: "plans".into(),
            allowed_tools: vec![],
        }];
        assert!(catalog_block(&defs).contains("(none)"));
    }
}
