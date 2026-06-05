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

/// Group rebuild paths by their detected [`origin_codegraph::Language`].
///
/// Each path's language is detected per-file via
/// [`origin_codegraph::Language::from_path`]; paths whose extension is not a
/// codegraph-supported grammar are dropped (skipped gracefully) rather than
/// forced through a wrong grammar. The returned groups are ordered by the
/// language discriminant for deterministic output, and within a group the
/// paths keep their input order.
///
/// This is the pure decision core behind the `graph_rebuild` tool dispatch:
/// the daemon calls `rebuild_paths` once per language group instead of
/// hardcoding a single grammar for the whole batch.
#[must_use]
pub fn group_paths_by_language(
    paths: &[std::path::PathBuf],
) -> Vec<(origin_codegraph::Language, Vec<std::path::PathBuf>)> {
    // Preserve first-seen language order while bucketing, then sort by the
    // stable discriminant so the result does not depend on input ordering.
    let mut groups: Vec<(origin_codegraph::Language, Vec<std::path::PathBuf>)> = Vec::new();
    for path in paths {
        let Some(lang) = origin_codegraph::Language::from_path(path) else {
            continue;
        };
        if let Some(slot) = groups.iter_mut().find(|(l, _)| *l == lang) {
            slot.1.push(path.clone());
        } else {
            groups.push((lang, vec![path.clone()]));
        }
    }
    groups.sort_by_key(|(l, _)| l.as_discriminant());
    groups
}

/// Maximum number of files the repo-map scanner walks, bounding the cost of the
/// (default-off) scan so an enormous tree can't stall prompt assembly.
const REPO_MAP_MAX_FILES: usize = 2_000;

/// Token budget for the rendered repo-map block (item E). Kept small so the
/// prepended map never dominates the system prompt.
const REPO_MAP_BUDGET_TOKENS: u32 = 1_024;

/// Build a compact `<repo-map>` system-prompt block spanning `roots` (item E).
///
/// Scans each root in `roots` for source files (via the 18-language
/// [`origin_repomap`] scanner), extracts lightweight per-file symbol defs/refs,
/// and ranks them with personalized `PageRank` inside
/// [`REPO_MAP_BUDGET_TOKENS`]. With a single root this is the original
/// single-corpus [`origin_repomap::build_map`] path; with more than one root the
/// per-root corpora are merged and re-ranked together via
/// [`origin_repomap::build_map_multi_root`] so cross-root references (root A
/// referencing a symbol root B defines) influence the shared ranking. Renders
/// the top files + their defined symbols. Returns `None` when no root has any
/// rankable source file or the ranker yields nothing, so an enabled-but-empty
/// workspace stays byte-neutral.
///
/// This is pure given the filesystem: it performs read-only directory walks and
/// never mutates anything. The caller (the agent loop) only invokes it behind
/// the `ORIGIN_REPOMAP=1` env gate, so the default prompt is unchanged.
#[must_use]
pub fn repo_map_block(roots: &[std::path::PathBuf]) -> Option<String> {
    let ranked = if roots.len() <= 1 {
        // Single root (or none): original single-corpus path.
        let root = roots.first()?;
        let files = scan_file_symbols(root);
        if files.is_empty() {
            return None;
        }
        repo_map_for(&files, &[], REPO_MAP_BUDGET_TOKENS).ok()?
    } else {
        // Multi-root: scan each root into its own corpus.
        let per_root: Vec<Vec<origin_repomap::FileSymbols>> =
            roots.iter().map(|r| scan_file_symbols(r)).collect();
        if per_root.iter().all(Vec::is_empty) {
            return None;
        }
        // ORIGIN_REPOMAP_PER_ROOT=1: rank each root INDEPENDENTLY (per-root
        // PageRank + per-root budget share), rendered as labelled sections, so a
        // small root is never buried by a large one and each root's most central
        // files always surface. Default (unset): merge + re-rank together under
        // one shared budget so cross-root edges are honoured — byte-identical to
        // before.
        if std::env::var("ORIGIN_REPOMAP_PER_ROOT").as_deref() == Ok("1") {
            return render_per_root_map(roots, &per_root);
        }
        origin_repomap::build_map_multi_root(per_root, &[], REPO_MAP_BUDGET_TOKENS).ok()?
    };
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

/// Render a `<repo-map>` block with one labelled section per workspace root.
///
/// Each root is ranked INDEPENDENTLY via [`origin_repomap::build_map_per_root`]
/// (gated by `ORIGIN_REPOMAP_PER_ROOT=1`): its own `PageRank` over only its own
/// graph plus an equal share of the token budget, so per-root locality is
/// preserved. Returns `None` when no root ranks anything.
fn render_per_root_map(
    roots: &[std::path::PathBuf],
    per_root: &[Vec<origin_repomap::FileSymbols>],
) -> Option<String> {
    let maps = origin_repomap::build_map_per_root(per_root, &[], REPO_MAP_BUDGET_TOKENS).ok()?;
    if maps.iter().all(|m| m.entries.is_empty()) {
        return None;
    }
    let mut out = String::from("<repo-map>\n");
    for m in &maps {
        out.push_str("# ");
        if let Some(p) = roots.get(m.root_index) {
            out.push_str(&p.display().to_string());
        } else {
            out.push_str("root ");
            out.push_str(&m.root_index.to_string());
        }
        out.push('\n');
        for entry in &m.entries {
            out.push_str("- ");
            out.push_str(&entry.file);
            if !entry.symbols.is_empty() {
                out.push_str(": ");
                out.push_str(&entry.symbols.join(", "));
            }
            out.push('\n');
        }
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
            // Source-file detection now spans all of origin_repomap's 18
            // languages (Ruby, PHP, Swift, Kotlin, …), not the old ~8-extension
            // list — so files the previous heuristic ignored still contribute.
            let is_source = path
                .to_str()
                .is_some_and(|p| origin_repomap::Language::from_path(p).is_some());
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
/// when the file can't be read as UTF-8, its language is unsupported, or it
/// yields no symbols.
///
/// Definitions come from [`origin_repomap::scan_path`] — the tested
/// 18-language, per-grammar definition scanner — giving full language breadth
/// and far better defs than the old inline `starts_with` heuristic. References
/// remain the cheap identifier-token set (used only to seed the def→ref ranking
/// graph), minus the file's own definitions so a file does not "reference"
/// itself.
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
    // Real per-language definition extraction (18 grammars). `scan_path` keys
    // off the path's extension; `None` means an unsupported language.
    let (_lang, defs) = origin_repomap::scan_path(&rel, &text)?;
    // Build the reference set from identifier tokens for the ranking graph,
    // excluding the file's own definitions.
    let def_set: std::collections::HashSet<&str> = defs.iter().map(String::as_str).collect();
    let mut refs = Vec::new();
    let mut approx_tokens: u32 = 0;
    for line in text.lines() {
        for tok in identifiers(line) {
            approx_tokens = approx_tokens.saturating_add(1);
            if !def_set.contains(tok.as_str()) {
                refs.push(tok);
            }
        }
    }
    let mut defs = defs;
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
    fn group_paths_by_language_detects_per_file_and_skips_unknown() {
        use std::path::PathBuf;
        let paths = vec![
            PathBuf::from("src/a.rs"),
            PathBuf::from("svc/b.py"),
            PathBuf::from("README.md"), // unsupported -> skipped
            PathBuf::from("src/c.rs"),
            PathBuf::from("data.bin"), // unsupported -> skipped
            PathBuf::from("svc/d.go"),
        ];
        let groups = group_paths_by_language(&paths);
        // Three supported languages: Rust, Python, Go. Markdown/bin dropped.
        assert_eq!(groups.len(), 3, "only supported languages form groups");
        let rust = groups
            .iter()
            .find(|(l, _)| *l == origin_codegraph::Language::Rust)
            .expect("rust group");
        assert_eq!(rust.1.len(), 2, "both .rs files land in the Rust group");
        let py = groups
            .iter()
            .find(|(l, _)| *l == origin_codegraph::Language::Python)
            .expect("python group");
        assert_eq!(py.1, vec![PathBuf::from("svc/b.py")]);
        let go = groups
            .iter()
            .find(|(l, _)| *l == origin_codegraph::Language::Go)
            .expect("go group");
        assert_eq!(go.1, vec![PathBuf::from("svc/d.go")]);
    }

    #[test]
    fn group_paths_by_language_empty_when_all_unsupported() {
        use std::path::PathBuf;
        let paths = vec![PathBuf::from("a.txt"), PathBuf::from("b.md"), PathBuf::from("noext")];
        assert!(group_paths_by_language(&paths).is_empty());
    }

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
    fn render_per_root_map_labels_each_root_section() {
        use std::path::PathBuf;
        let per_root = vec![
            vec![origin_repomap::FileSymbols::new(
                "a/core.rs",
                vec!["A".to_string()],
                vec![],
                10,
            )],
            vec![origin_repomap::FileSymbols::new(
                "b/core.rs",
                vec!["B".to_string()],
                vec![],
                10,
            )],
        ];
        let roots = vec![PathBuf::from("/ws/a"), PathBuf::from("/ws/b")];
        let block = render_per_root_map(&roots, &per_root).expect("non-empty per-root map");
        assert!(block.starts_with("<repo-map>\n"));
        assert!(block.trim_end().ends_with("</repo-map>"));
        // Both roots get a labelled section listing only their own file.
        assert!(block.contains("# "), "per-root sections are labelled");
        assert!(block.contains("a/core.rs: A"), "root A's file in its section");
        assert!(block.contains("b/core.rs: B"), "root B's file in its section");
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
        let block = repo_map_block(std::slice::from_ref(&dir)).expect("repo map block");
        assert!(block.starts_with("<repo-map>"));
        assert!(block.ends_with("</repo-map>"));
        assert!(block.contains("a.rs") || block.contains("b.py"));
        // A non-source-only directory yields no block.
        let empty = std::env::temp_dir().join(format!("origin_repomap_empty_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&empty);
        std::fs::write(empty.join("notes.txt"), "hi\n").expect("write txt");
        assert!(repo_map_block(std::slice::from_ref(&empty)).is_none());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn scan_file_symbols_covers_ruby_and_php_beyond_old_heuristic() {
        // The previous inline heuristic only knew ~8 C-family/Rust/Python/TS
        // extensions and ignored .rb / .php entirely. With origin_repomap's
        // 18-language scanner these files must now contribute defs.
        let dir = std::env::temp_dir().join(format!("origin_repomap_rbphp_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("widget.rb"),
            "def build_widget\n  assemble\nend\nclass Gadget\nend\n",
        )
        .expect("write rb");
        std::fs::write(
            dir.join("service.php"),
            "<?php\nfunction handle_request() {\n  return dispatch();\n}\n",
        )
        .expect("write php");
        let syms = scan_file_symbols(&dir);
        let rb = syms
            .iter()
            .find(|s| s.file == "widget.rb")
            .expect("ruby file scanned");
        assert!(
            rb.defines.contains(&"build_widget".to_string()),
            "ruby def must be extracted, got {:?}",
            rb.defines
        );
        assert!(
            rb.defines.contains(&"Gadget".to_string()),
            "ruby class must be extracted, got {:?}",
            rb.defines
        );
        let php = syms
            .iter()
            .find(|s| s.file == "service.php")
            .expect("php file scanned");
        assert!(
            php.defines.contains(&"handle_request".to_string()),
            "php function must be extracted, got {:?}",
            php.defines
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repo_map_block_multi_root_merges_both_roots() {
        let base = std::env::temp_dir().join(format!("origin_repomap_multi_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root_a = base.join("a");
        let root_b = base.join("b");
        std::fs::create_dir_all(&root_a).expect("mkdir a");
        std::fs::create_dir_all(&root_b).expect("mkdir b");
        // Root A defines `core_engine`; Root B references it (cross-root edge).
        std::fs::write(root_a.join("core.rb"), "def core_engine\n  spin\nend\n").expect("write a");
        std::fs::write(
            root_b.join("client.php"),
            "<?php\nfunction client_main() {\n  core_engine();\n}\n",
        )
        .expect("write b");
        let roots = vec![root_a.clone(), root_b];
        let block = repo_map_block(&roots).expect("multi-root map block");
        assert!(block.starts_with("<repo-map>"));
        assert!(
            block.contains("core.rb"),
            "root A file must appear in merged map: {block}"
        );
        assert!(
            block.contains("client.php"),
            "root B file must appear in merged map: {block}"
        );
        // A single-root call still works (default path) and is a subset.
        let single = repo_map_block(std::slice::from_ref(&root_a)).expect("single-root block");
        assert!(single.contains("core.rb"));
        let _ = std::fs::remove_dir_all(&base);
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
