// SPDX-License-Identifier: Apache-2.0
//! Additive, default-off bridges from the daemon into the Wave-1/2 subsystem
//! crates (Task 7).
//!
//! Each function is a thin, genuinely-reachable entry point into one crate
//! (`origin-repomap`, `origin-multimodal`, `origin-lspfleet`, `origin-review`)
//! so the dependency is exercised from the daemon rather than only declared.
//! None of these are invoked on the default agent path — they are available
//! helpers the daemon (and future surfaces) can call without re-deriving the
//! call shape. Keeping them here, behind plain function calls, means the agent
//! loop is untouched and default behavior is byte-identical.

/// Build a token-budgeted, `PageRank`-ordered repo map (origin-repomap).
///
/// `symbols` is the per-file def/ref extraction, `focus` biases the ranking
/// toward files referencing those symbols, and `token_budget` bounds the map.
///
/// # Errors
/// Propagates [`origin_repomap::RepoMapError`] when there are no files to rank.
pub fn repo_map_for(
    symbols: &[origin_repomap::FileSymbols],
    focus: &[String],
    token_budget: u32,
) -> Result<Vec<origin_repomap::RankedEntry>, origin_repomap::RepoMapError> {
    origin_repomap::build_map(symbols, focus, token_budget)
}

/// Convert raw file bytes into a provider-agnostic multimodal content block
/// (origin-multimodal), e.g. for image/PDF context attachment.
///
/// # Errors
/// Propagates [`origin_multimodal::MediaError`] for an undecodable or
/// unsupported media type.
pub fn content_block_for(
    bytes: &[u8],
    filename: &str,
) -> Result<origin_multimodal::ContentBlock, origin_multimodal::MediaError> {
    origin_multimodal::to_content_block(bytes, Some(filename))
}

/// Resolve the LSP server that handles a file extension (origin-lspfleet).
///
/// `ext` is the extension without a leading dot; matching is case-insensitive.
/// Returns `None` when no registered server claims the extension.
#[must_use]
pub fn lsp_for_ext(ext: &str) -> Option<&'static origin_lspfleet::LspServer> {
    origin_lspfleet::server_for_extension(ext)
}

/// Filter review findings down to those meeting a strictness threshold
/// (origin-review). Used by review surfaces to drop low-confidence noise.
#[must_use]
pub fn review_filter(
    findings: &[origin_review::Finding],
    strictness: origin_review::Strictness,
) -> Vec<origin_review::Finding> {
    origin_review::filter(findings, strictness)
}

/// Source-file extensions the repo-map scanner considers.
const REPO_MAP_EXTS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "go", "java", "c", "h", "cpp", "hpp",
];

/// Maximum number of files the repo-map scanner walks, bounding the cost of the
/// (default-off) scan so an enormous tree can't stall prompt assembly.
const REPO_MAP_MAX_FILES: usize = 2_000;

/// Token budget for the rendered repo-map block (item E). Kept small so the
/// prepended map never dominates the system prompt.
const REPO_MAP_BUDGET_TOKENS: u32 = 1_024;

/// Build a compact `<repo-map>` system-prompt block for `root` (item E).
///
/// Scans `root` for source files, extracts lightweight per-file symbol
/// defs/refs, ranks them with [`repo_map_for`] (personalized `PageRank`) inside
/// [`REPO_MAP_BUDGET_TOKENS`], and renders the top files + their defined
/// symbols. Returns `None` when the tree has no rankable source files or the
/// ranker yields nothing, so an enabled-but-empty repo stays byte-neutral.
///
/// This is pure given the filesystem: it performs read-only directory walks and
/// never mutates anything. The caller (the agent loop) only invokes it behind
/// the `ORIGIN_REPOMAP=1` env gate, so the default prompt is unchanged.
#[must_use]
pub fn repo_map_block(root: &std::path::Path) -> Option<String> {
    let files = scan_file_symbols(root);
    if files.is_empty() {
        return None;
    }
    let ranked = repo_map_for(&files, &[], REPO_MAP_BUDGET_TOKENS).ok()?;
    if ranked.is_empty() {
        return None;
    }
    let mut out = String::from("<repo-map>\n");
    for entry in &ranked {
        out.push_str("- ");
        out.push_str(&entry.file);
        if !entry.symbols.is_empty() {
            out.push_str(": ");
            out.push_str(&entry.symbols.join(", "));
        }
        out.push('\n');
    }
    out.push_str("</repo-map>");
    Some(out)
}

/// Walk `root` (bounded, skipping hidden / vendor dirs) and extract lightweight
/// [`origin_repomap::FileSymbols`] for each source file: identifier-like tokens
/// from `fn`/`def`/`class`/`struct`/`type`-ish lines become defs; all other
/// identifier tokens become refs. This is a cheap, dependency-free stand-in for
/// full tree-sitter extraction — enough to seed the `PageRank` ranking.
fn scan_file_symbols(root: &std::path::Path) -> Vec<origin_repomap::FileSymbols> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= REPO_MAP_MAX_FILES {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.')
                || matches!(
                    name.as_ref(),
                    "target" | "node_modules" | "vendor" | "dist" | "build"
                )
            {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let is_source = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| REPO_MAP_EXTS.contains(&e));
            if !is_source {
                continue;
            }
            if let Some(sym) = file_symbols(root, &path) {
                out.push(sym);
                if out.len() >= REPO_MAP_MAX_FILES {
                    break;
                }
            }
        }
        if out.len() >= REPO_MAP_MAX_FILES {
            break;
        }
    }
    out
}

/// Extract [`origin_repomap::FileSymbols`] from a single file. Returns `None`
/// when the file can't be read as UTF-8 or yields no symbols.
fn file_symbols(
    root: &std::path::Path,
    path: &std::path::Path,
) -> Option<origin_repomap::FileSymbols> {
    let text = std::fs::read_to_string(path).ok()?;
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let mut defs = Vec::new();
    let mut refs = Vec::new();
    let mut approx_tokens: u32 = 0;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let is_def = trimmed.starts_with("fn ")
            || trimmed.starts_with("pub fn ")
            || trimmed.starts_with("def ")
            || trimmed.starts_with("class ")
            || trimmed.starts_with("struct ")
            || trimmed.starts_with("pub struct ")
            || trimmed.starts_with("type ")
            || trimmed.starts_with("function ");
        for tok in identifiers(line) {
            approx_tokens = approx_tokens.saturating_add(1);
            if is_def {
                defs.push(tok);
            } else {
                refs.push(tok);
            }
        }
    }
    if defs.is_empty() && refs.is_empty() {
        return None;
    }
    defs.sort_unstable();
    defs.dedup();
    refs.sort_unstable();
    refs.dedup();
    defs.truncate(64);
    refs.truncate(128);
    // Approximate the rendered cost of this file in the map. Clamp to at least 1
    // so every admitted file contributes to the budget.
    let tokens = approx_tokens.clamp(1, 256);
    Some(origin_repomap::FileSymbols::new(rel, defs, refs, tokens))
}

/// Extract identifier-like tokens (alphanumeric/underscore, not starting with a
/// digit, length >= 3) from a line of source.
fn identifiers(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in line.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else {
            push_identifier(&mut out, &mut cur);
        }
    }
    push_identifier(&mut out, &mut cur);
    out
}

/// Flush `cur` into `out` when it is a valid identifier token, then clear it.
fn push_identifier(out: &mut Vec<String>, cur: &mut String) {
    if cur.len() >= 3 && !cur.starts_with(|c: char| c.is_ascii_digit()) {
        out.push(std::mem::take(cur));
    } else {
        cur.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_map_ranks_files_within_budget() {
        let symbols = vec![
            origin_repomap::FileSymbols::new("core.rs", vec!["Engine".to_string()], vec![], 40),
            origin_repomap::FileSymbols::new("a.rs", vec![], vec!["Engine".to_string()], 30),
        ];
        let map = repo_map_for(&symbols, &[], 1000).expect("build_map");
        assert!(!map.is_empty(), "ranked map should contain entries");
        // The widely-referenced definer ranks first.
        assert_eq!(map[0].file, "core.rs");
    }

    #[test]
    fn identifiers_filters_short_and_numeric_tokens() {
        let ids = identifiers("let foo = bar(42, x, abc_def);");
        assert!(ids.contains(&"foo".to_string()));
        assert!(ids.contains(&"bar".to_string()));
        assert!(ids.contains(&"abc_def".to_string()));
        // Too short / numeric-leading tokens are dropped.
        assert!(!ids.contains(&"x".to_string()));
        assert!(!ids.contains(&"42".to_string()));
    }

    #[test]
    fn file_symbols_splits_defs_and_refs() {
        let dir = std::env::temp_dir().join(format!("origin_repomap_fs_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("sample.rs");
        std::fs::write(
            &file,
            "pub fn compute_total(items: &[Order]) -> u64 {\n    sum_orders(items)\n}\n",
        )
        .expect("write sample");
        let sym = file_symbols(&dir, &file).expect("symbols");
        assert_eq!(sym.file, "sample.rs");
        assert!(sym.defines.contains(&"compute_total".to_string()));
        assert!(sym.references.contains(&"sum_orders".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repo_map_block_renders_or_none() {
        let dir = std::env::temp_dir().join(format!("origin_repomap_blk_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("a.rs"), "fn alpha() { beta(); }\n").expect("write a");
        std::fs::write(dir.join("b.py"), "def gamma():\n    return delta()\n").expect("write b");
        std::fs::write(dir.join("ignore.txt"), "not source\n").expect("write txt");
        let block = repo_map_block(&dir).expect("repo map block");
        assert!(block.starts_with("<repo-map>"));
        assert!(block.ends_with("</repo-map>"));
        assert!(block.contains("a.rs") || block.contains("b.py"));
        // A non-source-only directory yields no block.
        let empty = std::env::temp_dir().join(format!("origin_repomap_empty_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&empty);
        std::fs::write(empty.join("notes.txt"), "hi\n").expect("write txt");
        assert!(repo_map_block(&empty).is_none());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn content_block_classifies_text_and_image() {
        let text = content_block_for(b"hello world", "notes.txt").expect("text block");
        assert_eq!(text.kind, "text");
        // PNG magic bytes classify as an image content block.
        let png_bytes = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let png = content_block_for(&png_bytes, "pic.png").expect("image block");
        assert_eq!(png.kind, "image");
        assert_eq!(png.media_type.as_deref(), Some("image/png"));
    }

    #[test]
    fn lsp_for_ext_resolves_rust_and_misses_unknown() {
        let rs = lsp_for_ext("rs").expect("rust server registered");
        assert_eq!(rs.server_id, "rust-analyzer");
        assert!(lsp_for_ext("totally-unknown-ext").is_none());
    }

    #[test]
    fn review_filter_drops_low_confidence_under_strict() {
        let findings = vec![
            origin_review::Finding::new(
                origin_review::Dimension::Bug,
                "a.rs",
                1,
                "high-confidence bug",
                "",
                0.95,
            ),
            origin_review::Finding::new(
                origin_review::Dimension::Style,
                "a.rs",
                2,
                "low-confidence nit",
                "",
                0.1,
            ),
        ];
        let kept = review_filter(&findings, origin_review::Strictness::Strict);
        assert_eq!(kept.len(), 1, "only the high-confidence finding survives Strict");
        assert_eq!(kept[0].line, 1);
    }
}
