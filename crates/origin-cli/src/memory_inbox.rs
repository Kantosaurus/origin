// SPDX-License-Identifier: Apache-2.0
//! `origin memory inbox` — list / accept / reject the auto-memory drafts the
//! daemon's mem-garden stages under `~/.origin/memory-inbox/`.
//!
//! This is the consumer for the producer-only mem-garden: `list` and `reject`
//! are pure filesystem ops; `accept` promotes a draft into the live memory store
//! via the same `MemoryHandle::save` primitive the in-session `/mem` accept uses.
//! Distinct from the `/mem` numeric proposal queue.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::RwLock;

use crate::cli_def::{MemoryInboxSub, MemorySub};

/// A parsed auto-memory draft.
struct Draft {
    /// Filename stem (the content-hash key).
    id: String,
    path: PathBuf,
    tags: Vec<String>,
    body: String,
}

/// `~/.origin/memory-inbox`, honoring `ORIGIN_HOME` exactly like the producer in
/// `origin-daemon::mem_garden` so the CLI and daemon agree on the directory.
fn inbox_dir() -> Result<PathBuf> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .context("cannot resolve home directory (set ORIGIN_HOME)")?;
    Ok(home.join(".origin").join("memory-inbox"))
}

/// Pure parse of a draft's markdown (trivial daemon-generated frontmatter + body).
/// Tolerant: a missing/garbled frontmatter just yields empty session/tags and the
/// whole text as the body.
fn parse_draft_content(id: &str, raw: &str) -> Draft {
    let mut tags = Vec::new();
    let body = split_frontmatter(raw, &mut tags);
    Draft {
        id: id.to_string(),
        path: PathBuf::new(),
        tags,
        body,
    }
}

/// Extract the body and fill `tags` from a draft's trivial frontmatter. Tolerant:
/// missing or garbled frontmatter yields empty tags and the whole text as body.
fn split_frontmatter(raw: &str, tags: &mut Vec<String>) -> String {
    let Some(rest) = raw.strip_prefix("---\n") else {
        return raw.trim_end().to_string();
    };
    let Some(end) = rest.find("\n---\n") else {
        return raw.trim_end().to_string();
    };
    for line in rest[..end].lines() {
        if let Some(v) = line.strip_prefix("tags:") {
            let inner = v.trim().trim_start_matches('[').trim_end_matches(']');
            *tags = inner
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
        }
    }
    rest[end + 5..].trim_start_matches('\n').trim_end().to_string()
}

fn parse_draft(path: &Path) -> Result<Draft> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut d = parse_draft_content(&id, &raw);
    d.path = path.to_path_buf();
    Ok(d)
}

fn load_drafts() -> Result<Vec<Draft>> {
    let dir = inbox_dir()?;
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(out); // no inbox dir yet == empty
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Ok(d) = parse_draft(&p) {
                out.push(d);
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Resolve a draft by exact id or a unique prefix (git-short-sha style), since
/// the 16-hex content-hash ids are unwieldy to type in full.
fn resolve<'a>(drafts: &'a [Draft], id: &str) -> Result<&'a Draft> {
    let matches: Vec<&Draft> = drafts.iter().filter(|d| d.id == id || d.id.starts_with(id)).collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => anyhow::bail!("no draft matches id `{id}`"),
        many => anyhow::bail!("ambiguous id `{id}` matches {} drafts", many.len()),
    }
}

fn list_drafts() -> Result<String> {
    use std::fmt::Write as _;
    let drafts = load_drafts()?;
    if drafts.is_empty() {
        return Ok("memory inbox is empty".to_string());
    }
    let mut out = format!("{} draft(s) in the memory inbox:\n", drafts.len());
    for d in &drafts {
        let preview: String = d.body.lines().next().unwrap_or_default().chars().take(70).collect();
        let tags = if d.tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", d.tags.join(","))
        };
        let _ = writeln!(out, "  {}{tags}  {preview}", d.id);
    }
    Ok(out)
}

fn reject_draft(id: &str) -> Result<String> {
    let drafts = load_drafts()?;
    let d = resolve(&drafts, id)?;
    std::fs::remove_file(&d.path).with_context(|| format!("removing {}", d.path.display()))?;
    Ok(format!("rejected {}", d.id))
}

fn accept_draft(id: &str) -> Result<String> {
    use origin_tools::dispatch::MemoryHandle as _;
    let drafts = load_drafts()?;
    let d = resolve(&drafts, id)?;
    let handle = open_store_handle()?;
    let memid = handle
        .save(&d.body, &d.tags)
        .map_err(|e| anyhow::anyhow!("saving memory: {e:?}"))?;
    std::fs::remove_file(&d.path).with_context(|| format!("removing {}", d.path.display()))?;
    Ok(format!("accepted {} -> memory {memid}", d.id))
}

/// Open the daemon's own `SQLite` + CAS stores (`WAL` makes concurrent write
/// with a running daemon safe) and build a `MemoryHandle`. Mirrors the daemon's
/// `MemoryWiring` construction; no embedder, so save uses the fallback quantizer.
fn open_store_handle() -> Result<Arc<origin_daemon::MemoryDispatchHandle>> {
    let db_path = std::env::var("ORIGIN_DB").unwrap_or_else(|_| {
        let mut p = std::env::temp_dir();
        p.push("origin.db");
        p.to_string_lossy().into_owned()
    });
    let cas_root: PathBuf = std::env::var_os("ORIGIN_CAS_ROOT").map_or_else(
        || {
            let mut p = std::env::temp_dir();
            p.push("origin-cas");
            p
        },
        PathBuf::from,
    );
    let cas = Arc::new(
        origin_cas::Store::open(origin_cas::StoreConfig {
            root: cas_root,
            hot_capacity: 256,
            warm_pack_target_bytes: 4 << 20,
            cold_zstd_level: 3,
        })
        .context("opening CAS store")?,
    );
    let sql = Arc::new(origin_store::Store::open(&db_path).context("opening SQLite store")?);
    let store = Arc::new(origin_mem::MemoryStore::new(sql, cas));
    let wiring = origin_daemon::MemoryWiring::new(
        store,
        None,
        Arc::new(RwLock::new(origin_mem::MemIndex::new())),
    );
    Ok(wiring.handle())
}

/// Dispatch `origin memory <sub>`.
///
/// # Errors
/// Propagates filesystem errors and (for `accept`) memory-store errors.
pub fn run_memory(sub: MemorySub) -> Result<()> {
    let MemorySub::Inbox { sub } = sub;
    let msg = match sub {
        MemoryInboxSub::List => list_drafts()?,
        MemoryInboxSub::Accept { id } => accept_draft(&id)?,
        MemoryInboxSub::Reject { id } => reject_draft(&id)?,
    };
    println!("{msg}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_draft_content;

    #[test]
    fn parses_frontmatter_session_tags_and_body() {
        let raw = "---\nsource: auto-memory\nsession: \"sess-1\"\ntags: [tone, style]\nhash: abc123\n---\n\nuser prefers terse replies\n";
        let d = parse_draft_content("abc123", raw);
        assert_eq!(d.tags, vec!["tone".to_string(), "style".to_string()]);
        assert_eq!(d.body, "user prefers terse replies");
        assert_eq!(d.id, "abc123");
    }

    #[test]
    fn parses_empty_tags_list() {
        let raw = "---\nsource: auto-memory\nsession: \"s\"\ntags: []\nhash: x\n---\n\nbody text\n";
        let d = parse_draft_content("x", raw);
        assert!(d.tags.is_empty());
        assert_eq!(d.body, "body text");
    }

    #[test]
    fn tolerates_missing_frontmatter() {
        let d = parse_draft_content("id", "just a body, no frontmatter");
        assert_eq!(d.body, "just a body, no frontmatter");
        assert!(d.tags.is_empty());
    }
}
