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
    /// Inline MCP servers this sub-agent runs for its turn (gap 9b; the `mcp:`
    /// frontmatter field). Empty for most sub-agents.
    pub mcp_servers: Vec<origin_swarm::McpServerSpec>,
}

/// The subagent-local frontmatter fields the shared `SkillFrontmatter` parser
/// does not model — currently just the `mcp:` server list (gap 9b). Parsed
/// separately so the skill parser stays unchanged.
#[derive(Debug, Default, serde::Deserialize)]
struct SubagentExtra {
    #[serde(default)]
    mcp: Vec<origin_swarm::McpServerSpec>,
}

/// Extract the `mcp:` server list from a subagent `.md`'s YAML frontmatter.
/// Tolerant: missing frontmatter or no `mcp:` key yields an empty list.
fn parse_mcp(raw: &str) -> Vec<origin_swarm::McpServerSpec> {
    let normalized = raw.replace("\r\n", "\n");
    let stripped = normalized.strip_prefix('\u{FEFF}').unwrap_or(&normalized);
    let Some(rest) = stripped.strip_prefix("---\n") else {
        return Vec::new();
    };
    let Some((yaml, _body)) = rest.split_once("\n---\n") else {
        return Vec::new();
    };
    serde_yaml::from_str::<SubagentExtra>(yaml)
        .map(|e| e.mcp)
        .unwrap_or_default()
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
                mcp_servers: parse_mcp(&raw),
            }),
            Ok(_) => {}
            Err(e) => tracing::warn!(path = %path.display(), error = %e, "subagents: skipping malformed .md"),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The tools the built-in `browser` subagent is scoped to: the network-capable
/// browse tools plus read-only inspection. Never `Edit`/`Write`/`Bash`/`Task`.
const BROWSER_SUBAGENT_TOOLS: &[&str] = &["Browser", "WebFetch", "WebSearch", "Read", "Grep", "Glob"];

/// Build the first-class, built-in `browser` named subagent (browser-security
/// C), scoped to the browse/read tools and carrying the active conseca
/// domain-allowlist in its description.
///
/// Returns `None` when `allow_domains` is empty so that — exactly as for
/// `.md`-defined subagents — the assembled system prompt (and its cache
/// breakpoints) stays byte-identical to before whenever the browser-security
/// allowlist is unconfigured. When non-empty, the returned [`SubagentDef`] reuses
/// the same `WorkerSpec`/`allowed_tools` enforcement the swarm worker applies, so
/// the model can dispatch a `browser` subagent that is genuinely confined to the
/// allow-listed domains and the browse/read tool set.
#[must_use]
pub fn builtin_browser_subagent(allow_domains: &[String]) -> Option<SubagentDef> {
    if allow_domains.is_empty() {
        return None;
    }
    Some(SubagentDef {
        name: "browser".to_string(),
        description: format!(
            "Isolated web-browsing agent confined to the allow-listed domains \
             [{}] and the browse/read tools; it cannot edit files or run shell.",
            allow_domains.join(", ")
        ),
        allowed_tools: BROWSER_SUBAGENT_TOOLS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        mcp_servers: Vec::new(),
    })
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
    // gap 9b: only when a sub-agent declares inline MCP servers, instruct the
    // model to forward them — so sessions with no MCP sub-agents stay byte-identical.
    if defs.iter().any(|d| !d.mcp_servers.is_empty()) {
        out.push_str(
            "A sub-agent may also list `mcp_servers`; when you delegate to it, copy that JSON \
             verbatim into the `Task` call's `mcp_servers` field.\n",
        );
    }
    for d in defs {
        let tools = if d.allowed_tools.is_empty() {
            "(none)".to_string()
        } else {
            d.allowed_tools.join(",")
        };
        let _ = writeln!(out, "  - {} — [{}] — {}", d.name, tools, d.description);
        if !d.mcp_servers.is_empty() {
            let mcp_json = serde_json::to_string(&d.mcp_servers).unwrap_or_default();
            let _ = writeln!(out, "      mcp_servers: {mcp_json}");
        }
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

/// The process-wide `.md`-loaded subagent defs, cached once on first use.
fn global_defs() -> &'static [SubagentDef] {
    static CELL: OnceLock<Vec<SubagentDef>> = OnceLock::new();
    CELL.get_or_init(|| subagents_dir().map_or_else(Vec::new, |d| load(&d)))
        .as_slice()
}

/// Render the `<origin-subagents>` block from the `.md`-loaded subagents PLUS
/// the built-in `browser` subagent when `allow_domains` is non-empty
/// (browser-security C).
///
/// **Default-off / byte-identical:** when `allow_domains` is empty AND there are
/// no `.md` subagents, this returns `""` — identical to [`global_block`]. With an
/// empty allowlist it is byte-identical to [`global_block`] regardless of `.md`
/// files (the built-in is simply not injected), so the only behavioral change is
/// the additive injection of one extra catalog line when a browser allowlist is
/// configured.
#[must_use]
pub fn block_with_builtins(allow_domains: &[String]) -> String {
    let md_defs = global_defs();
    builtin_browser_subagent(allow_domains).map_or_else(
        || catalog_block(md_defs),
        |browser| {
            let mut defs: Vec<SubagentDef> = md_defs.to_vec();
            defs.push(browser);
            defs.sort_by(|a, b| a.name.cmp(&b.name));
            catalog_block(&defs)
        },
    )
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
    fn builtin_browser_subagent_scoped_and_carries_allowlist() {
        use super::builtin_browser_subagent;
        // With no allowlist configured ⇒ no built-in browser subagent (the
        // default system prompt stays byte-identical).
        assert!(builtin_browser_subagent(&[]).is_none());

        // With an allowlist ⇒ a named `browser` subagent scoped to the
        // browse/read tools, carrying the allowlist in its description.
        let def = builtin_browser_subagent(&["example.com".to_string(), "docs.rs".to_string()])
            .expect("allowlist ⇒ built-in browser subagent");
        assert_eq!(def.name, "browser");
        // Scoped to exactly the browse + read tools, never Edit/Write/Bash/Task.
        assert!(def.allowed_tools.contains(&"Browser".to_string()));
        assert!(def.allowed_tools.contains(&"WebFetch".to_string()));
        assert!(def.allowed_tools.contains(&"WebSearch".to_string()));
        assert!(def.allowed_tools.contains(&"Read".to_string()));
        assert!(!def.allowed_tools.contains(&"Edit".to_string()));
        assert!(!def.allowed_tools.contains(&"Bash".to_string()));
        assert!(!def.allowed_tools.contains(&"Task".to_string()));
        // The allowlist domains are surfaced in the description.
        assert!(def.description.contains("example.com"));
        assert!(def.description.contains("docs.rs"));
    }

    #[test]
    fn block_lists_tools_or_none() {
        let defs = vec![SubagentDef {
            name: "planner".into(),
            description: "plans".into(),
            allowed_tools: vec![],
            mcp_servers: vec![],
        }];
        assert!(catalog_block(&defs).contains("(none)"));
    }

    #[test]
    fn parses_mcp_frontmatter_and_lists_it_in_block() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("gh.md"),
            "---\nname: gh\ndescription: github helper\nallowed-tools: [Read, mcp__github__*]\nmcp:\n  - name: github\n    command: gh-mcp\n    args: [--stdio]\n---\nYou use the GitHub MCP server.\n",
        )
        .unwrap();
        let defs = load(dir.path());
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].mcp_servers.len(), 1);
        assert_eq!(defs[0].mcp_servers[0].name, "github");
        assert_eq!(defs[0].mcp_servers[0].command.as_deref(), Some("gh-mcp"));
        // The block forwards the mcp servers to the model and the allow-list glob.
        let block = catalog_block(&defs);
        assert!(block.contains("mcp_servers"));
        assert!(block.contains("github"));
    }

    #[test]
    fn no_mcp_keeps_block_free_of_mcp_instruction() {
        let defs = vec![SubagentDef {
            name: "explorer".into(),
            description: "reads".into(),
            allowed_tools: vec!["Read".into()],
            mcp_servers: vec![],
        }];
        assert!(!catalog_block(&defs).contains("mcp_servers"));
    }
}
