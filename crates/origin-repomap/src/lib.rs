// SPDX-License-Identifier: Apache-2.0
//! Token-budgeted repository map via personalized `PageRank` over a symbol graph.
//!
//! `origin`'s code-intelligence pipeline extracts a symbol graph (which file
//! *defines* and which file *references* each symbol) with tree-sitter upstream
//! in `origin-codegraph`. This crate is the *ranker*: it turns that raw graph
//! into the most context-worthy slice of the repository, packed to fit a token
//! budget — the "repo map" trick popularized by `aider`.
//!
//! The mechanism is personalized `PageRank` on a directed file graph: file `A`
//! points at file `B` when `A` references a symbol that `B` defines. Widely
//! depended-upon definers (config, core types, hot utilities) bubble to the top,
//! and a `focus` set biases the random-restart vector toward the files the user
//! is actively working on. [`build_map`] then greedily admits the top-ranked
//! files until the token budget is exhausted.
//!
//! The crate is pure — no I/O, no async, no tree-sitter — so it is trivially
//! unit-testable and deterministic. Tie-breaks are resolved by file name so a
//! given graph always yields the same ordering.
//!
//! ```
//! use origin_repomap::{build_map, FileSymbols};
//!
//! let files = vec![
//!     FileSymbols::new("core.rs", vec!["Engine".into()], vec![], 40),
//!     FileSymbols::new("a.rs", vec![], vec!["Engine".into()], 30),
//!     FileSymbols::new("b.rs", vec![], vec!["Engine".into()], 30),
//! ];
//! let map = build_map(&files, &[], 1_000).unwrap();
//! // The widely-referenced definer ranks first.
//! assert_eq!(map[0].file, "core.rs");
//! ```

#![forbid(unsafe_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors produced while building a repo map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RepoMapError {
    /// The input slice contained no files, so there is nothing to rank.
    #[error("no files supplied to rank")]
    Empty,
}

/// The symbols a single file defines and references, plus its map token cost.
///
/// This is the per-file row of the symbol graph emitted upstream by
/// `origin-codegraph`. `tokens` is the approximate cost of including this file's
/// portion of the rendered map (signatures + a few lines of context).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSymbols {
    /// Repo-relative file path; also the node identity and tie-break key.
    pub file: String,
    /// Symbol names this file defines (functions, types, constants, …).
    pub defines: Vec<String>,
    /// Symbol names this file references (calls, uses, mentions).
    pub references: Vec<String>,
    /// Approximate token cost of including this file in the rendered map.
    pub tokens: u32,
}

impl FileSymbols {
    /// Construct a file row.
    #[must_use]
    pub fn new(file: impl Into<String>, defines: Vec<String>, references: Vec<String>, tokens: u32) -> Self {
        Self {
            file: file.into(),
            defines,
            references,
            tokens,
        }
    }
}

/// One ranked file in the final repo map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankedEntry {
    /// Repo-relative file path.
    pub file: String,
    /// `PageRank` score (higher is more central to the codebase).
    pub score: f64,
    /// The symbols this file defines, surfaced for the rendered map.
    pub symbols: Vec<String>,
}

/// Rank files by `PageRank` over the def→ref file graph.
///
/// An edge `A -> B` is added whenever file `A` references a symbol that file `B`
/// defines, so importance flows toward the files that *define* widely-used
/// symbols. Returns `(file, score)` pairs sorted by descending score, with ties
/// broken by ascending file name for deterministic output. Self-references (a
/// file referencing a symbol it also defines) are ignored.
///
/// `damping` is the standard `PageRank` teleport factor (typically `0.85`) and is
/// clamped to `[0, 1]`; `iters` is the power-iteration count (e.g. `20`). An
/// empty input yields an empty vector.
#[must_use]
pub fn pagerank(files: &[FileSymbols], damping: f64, iters: u32) -> Vec<(String, f64)> {
    rank(files, &[], damping, iters)
}

/// Personalized `PageRank` biased toward the `focus` files.
///
/// Identical to [`pagerank`] except the random-restart (teleport) vector is
/// concentrated on the `focus` files instead of being uniform, pulling rank mass
/// toward the files the user is actively working on and their neighbourhood.
/// `focus` entries that do not name a known file are ignored; if none match, the
/// teleport vector falls back to uniform (degrading to plain `PageRank`).
///
/// Returns `(file, score)` pairs sorted by descending score with file-name
/// tie-breaking. An empty input yields an empty vector.
#[must_use]
pub fn personalized_pagerank(
    files: &[FileSymbols],
    focus: &[String],
    damping: f64,
    iters: u32,
) -> Vec<(String, f64)> {
    rank(files, focus, damping, iters)
}

/// Default `PageRank` teleport factor used by [`build_map`].
const DEFAULT_DAMPING: f64 = 0.85;
/// Default power-iteration count used by [`build_map`].
const DEFAULT_ITERS: u32 = 24;

/// Build a token-budgeted repo map: rank files, then greedily admit the
/// top-ranked ones until adding the next would exceed `token_budget`.
///
/// Ranking uses personalized `PageRank` biased toward `focus` (pass an empty slice
/// for an unbiased map). Files are considered in descending rank order; each is
/// included only if it fits in the remaining budget, so a single oversized file
/// never starves the smaller, also-relevant files behind it. A file whose own
/// `tokens` exceed the entire budget is skipped.
///
/// # Errors
///
/// Returns [`RepoMapError::Empty`] when `files` is empty.
pub fn build_map(
    files: &[FileSymbols],
    focus: &[String],
    token_budget: u32,
) -> Result<Vec<RankedEntry>, RepoMapError> {
    if files.is_empty() {
        return Err(RepoMapError::Empty);
    }

    let ranked = rank(files, focus, DEFAULT_DAMPING, DEFAULT_ITERS);
    let defines: HashMap<&str, &Vec<String>> = files.iter().map(|f| (f.file.as_str(), &f.defines)).collect();
    let costs: HashMap<&str, u32> = files.iter().map(|f| (f.file.as_str(), f.tokens)).collect();

    let mut out = Vec::new();
    let mut spent: u32 = 0;
    for (file, score) in ranked {
        let cost = costs.get(file.as_str()).copied().unwrap_or(0);
        if spent.saturating_add(cost) > token_budget {
            // Skip this file but keep scanning: a smaller, lower-ranked file may
            // still fit in the remaining budget.
            continue;
        }
        spent = spent.saturating_add(cost);
        let symbols = defines.get(file.as_str()).map_or_else(Vec::new, |d| (*d).clone());
        out.push(RankedEntry { file, score, symbols });
    }
    Ok(out)
}

/// Merge several workspace roots' symbol graphs into one corpus, then rank the
/// whole thing through [`build_map`] under a single shared token budget.
///
/// Each element of `per_root` is one root's `Vec<FileSymbols>` (as produced by
/// the scanner). The rows are concatenated *in root order* and de-duplicated by
/// [`FileSymbols::file`]: the first occurrence of a path wins and any later row
/// naming the same path is dropped wholesale (symbol sets are **not** unioned).
/// This keeps the corpus deterministic — a file shared by two roots contributes
/// exactly one node, seeded from the earliest root — while letting the existing
/// personalized `PageRank` re-rank the combined def→ref graph so cross-root
/// dependencies (root A referencing a symbol root B defines) influence the map.
///
/// Re-ranking is delegated entirely to [`build_map`]; this function only owns the
/// concatenate-and-dedup step. Identical input therefore yields identical output.
///
/// # Errors
///
/// Returns [`RepoMapError::Empty`] when the merged corpus contains no files
/// (every root was empty), matching [`build_map`]'s contract.
pub fn merge_and_rerank_maps(
    per_root: Vec<Vec<FileSymbols>>,
    focus: &[String],
    token_budget: u32,
) -> Result<Vec<RankedEntry>, RepoMapError> {
    let merged = dedup_corpus(per_root);
    if merged.is_empty() {
        return Err(RepoMapError::Empty);
    }
    build_map(&merged, focus, token_budget)
}

/// Build a token-budgeted repo map spanning several pre-scanned workspace roots.
///
/// This is the multi-root entry point. Because the crate is pure (no I/O, no
/// directory walking — see the crate-level docs), each root must already be
/// scanned into a `Vec<FileSymbols>` by the caller (e.g. via [`scan_path`]); the
/// roots are then merged and re-ranked together via [`merge_and_rerank_maps`],
/// so a single shared `token_budget` is split across all roots and cross-root
/// references are honoured by the ranker.
///
/// # Errors
///
/// Returns [`RepoMapError::Empty`] when `per_root` is empty or every root is
/// empty (the merged corpus has no files), matching [`build_map`]'s contract.
pub fn build_map_multi_root(
    per_root: Vec<Vec<FileSymbols>>,
    focus: &[String],
    token_budget: u32,
) -> Result<Vec<RankedEntry>, RepoMapError> {
    if per_root.is_empty() {
        return Err(RepoMapError::Empty);
    }
    merge_and_rerank_maps(per_root, focus, token_budget)
}

/// One workspace root's independently-ranked map (see [`build_map_per_root`]).
#[derive(Debug, Clone, PartialEq)]
pub struct RootMap {
    /// Index of this root in the input `per_root` slice.
    pub root_index: usize,
    /// The root's files, ranked by personalized `PageRank` over ONLY that root's
    /// own def→ref graph, admitted under the root's share of the token budget.
    pub entries: Vec<RankedEntry>,
}

/// Rank each workspace root INDEPENDENTLY instead of merging them.
///
/// Runs personalized `PageRank` over each root's own symbol graph under its own
/// share of the token budget (vs [`build_map_multi_root`], which merges every
/// root into one corpus and ranks globally).
///
/// Per-root ranking preserves within-root locality: a small root is never buried
/// by a large one, each root's most central files always appear in its own
/// section of the map, and cross-root edges cannot dilute a root's internal
/// importance ordering. The shared `token_budget` is split evenly across the
/// NON-EMPTY roots, with the remainder handed to the earliest roots so the whole
/// budget is allocated. `focus` only biases the root that actually contains a
/// focus file (the ranker ignores focus entries naming files absent from a given
/// root, per [`personalized_pagerank`]).
///
/// Returns one [`RootMap`] per non-empty input root, in input order.
///
/// # Errors
///
/// Returns [`RepoMapError::Empty`] when every root is empty.
pub fn build_map_per_root(
    per_root: &[Vec<FileSymbols>],
    focus: &[String],
    token_budget: u32,
) -> Result<Vec<RootMap>, RepoMapError> {
    let non_empty: Vec<usize> = per_root
        .iter()
        .enumerate()
        .filter(|(_, files)| !files.is_empty())
        .map(|(i, _)| i)
        .collect();
    if non_empty.is_empty() {
        return Err(RepoMapError::Empty);
    }
    let n = u32::try_from(non_empty.len()).unwrap_or(u32::MAX).max(1);
    let base = token_budget / n;
    let mut remainder = token_budget % n;

    let mut out = Vec::with_capacity(non_empty.len());
    for &i in &non_empty {
        // Earliest roots absorb the +1 remainder so the entire budget is spent.
        let mut budget = base;
        if remainder > 0 {
            budget = budget.saturating_add(1);
            remainder -= 1;
        }
        // `per_root[i]` is non-empty by construction, so `build_map` never
        // returns `Empty` here.
        let entries = build_map(&per_root[i], focus, budget)?;
        out.push(RootMap {
            root_index: i,
            entries,
        });
    }
    Ok(out)
}

/// Concatenate per-root rows into one corpus, dropping later duplicates by path.
///
/// First occurrence of each [`FileSymbols::file`] wins; subsequent rows naming a
/// path already seen are discarded (their symbol sets are *not* merged). Order is
/// the stable concatenation order of `per_root`, so the result is deterministic.
fn dedup_corpus(per_root: Vec<Vec<FileSymbols>>) -> Vec<FileSymbols> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut merged: Vec<FileSymbols> = Vec::new();
    for root in per_root {
        for f in root {
            if seen.insert(f.file.clone()) {
                merged.push(f);
            }
        }
    }
    merged
}

/// Core ranker shared by the public entry points.
///
/// Builds the def→ref adjacency, runs `iters` rounds of power iteration with the
/// given (optionally personalized) teleport vector, then returns scores sorted
/// descending with file-name tie-breaking.
fn rank(files: &[FileSymbols], focus: &[String], damping: f64, iters: u32) -> Vec<(String, f64)> {
    let n = files.len();
    if n == 0 {
        return Vec::new();
    }

    // Stable index per file (first occurrence wins on duplicate paths).
    let mut index: HashMap<&str, usize> = HashMap::with_capacity(n);
    for (i, f) in files.iter().enumerate() {
        index.entry(f.file.as_str()).or_insert(i);
    }

    // Map each defined symbol to the set of files that define it.
    let mut definers: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        for sym in &f.defines {
            let entry = definers.entry(sym.as_str()).or_default();
            if !entry.contains(&i) {
                entry.push(i);
            }
        }
    }

    // Adjacency: out_links[a] = files a points at (a references a symbol they define).
    let mut out_links: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (a, f) in files.iter().enumerate() {
        for sym in &f.references {
            if let Some(defs) = definers.get(sym.as_str()) {
                for &b in defs {
                    if a != b && !out_links[a].contains(&b) {
                        out_links[a].push(b);
                    }
                }
            }
        }
    }

    let teleport = teleport_vector(focus, &index, n);
    let scores = power_iterate(&out_links, &teleport, damping, iters, n);

    let mut ranked: Vec<(String, f64)> = files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.file.clone(), scores[i]))
        .collect();
    // Highest score first; ties broken by ascending file name for determinism.
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked
}

/// Build the teleport (restart) probability vector, summing to 1.
///
/// Uniform when `focus` is empty or names no known file; otherwise mass is split
/// evenly across the matched focus files.
fn teleport_vector(focus: &[String], index: &HashMap<&str, usize>, n: usize) -> Vec<f64> {
    #[allow(clippy::cast_precision_loss)] // n is far below 2^53.
    let uniform = 1.0 / n as f64;

    let mut matched: Vec<usize> = Vec::new();
    for name in focus {
        if let Some(&i) = index.get(name.as_str()) {
            if !matched.contains(&i) {
                matched.push(i);
            }
        }
    }

    if matched.is_empty() {
        return vec![uniform; n];
    }

    #[allow(clippy::cast_precision_loss)] // matched.len() <= n, far below 2^53.
    let share = 1.0 / matched.len() as f64;
    let mut v = vec![0.0; n];
    for i in matched {
        v[i] = share;
    }
    v
}

/// Run `iters` rounds of power iteration and return per-node scores summing to 1.
///
/// Dangling nodes (no out-links) redistribute their mass via the teleport
/// vector, so probability is conserved and the result is a proper distribution.
fn power_iterate(out_links: &[Vec<usize>], teleport: &[f64], damping: f64, iters: u32, n: usize) -> Vec<f64> {
    let d = damping.clamp(0.0, 1.0);
    let mut scores = teleport.to_vec();

    for _ in 0..iters {
        let mut next = vec![0.0; n];

        // Mass parked on dangling nodes is teleported (keeps the vector summing to 1).
        let mut dangling = 0.0;
        for (i, links) in out_links.iter().enumerate() {
            if links.is_empty() {
                dangling += scores[i];
            }
        }

        for (i, links) in out_links.iter().enumerate() {
            if links.is_empty() {
                continue;
            }
            #[allow(clippy::cast_precision_loss)] // out-degree is far below 2^53.
            let push = scores[i] / links.len() as f64;
            for &j in links {
                next[j] += push;
            }
        }

        for (i, slot) in next.iter_mut().enumerate() {
            let incoming = dangling.mul_add(teleport[i], *slot);
            *slot = (1.0 - d).mul_add(teleport[i], d * incoming);
        }
        scores = next;
    }

    scores
}

// ----------------------------------------------------------------------------
// Lightweight, dependency-free definition scanner.
// ----------------------------------------------------------------------------
//
// The `PageRank` ranker above consumes a pre-built symbol graph ([`FileSymbols`]).
// Upstream, `origin-codegraph` produces that graph with tree-sitter — accurate
// but heavyweight (one compiled grammar per language). For the *repo-map* fast
// path we want a scanner that needs no grammar at all: a cheap, line-oriented
// heuristic that recognizes definition "leaders" (`fn`, `def`, `class`, …) and
// pulls out the declared name. It is intentionally approximate — good enough to
// seed the def→ref graph for ranking, at zero dependency and near-zero cost.
//
// The scanner is pure string work: no `regex` crate, no tree-sitter, no I/O. It
// keys off a file extension → [`Language`] map and a per-language set of leaders.

/// Source languages the lightweight [`scan_definitions`] heuristic understands.
///
/// This is deliberately broader than the tree-sitter grammar set: because the
/// scanner is pure text heuristics, adding a language costs only a few match
/// arms, not a compiled grammar. Unknown extensions map to `None` and are simply
/// skipped by callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Language {
    /// Rust (`.rs`).
    Rust,
    /// TypeScript / JavaScript (`.ts`, `.tsx`, `.js`, `.jsx`, `.mjs`, `.cjs`).
    TypeScript,
    /// Python (`.py`, `.pyi`).
    Python,
    /// Go (`.go`).
    Go,
    /// Java (`.java`).
    Java,
    /// C (`.c`, `.h`).
    C,
    /// C++ (`.cc`, `.cpp`, `.cxx`, `.hpp`, `.hh`, `.hxx`).
    Cpp,
    /// C# (`.cs`).
    CSharp,
    /// Ruby (`.rb`).
    Ruby,
    /// PHP (`.php`).
    Php,
    /// Swift (`.swift`).
    Swift,
    /// Kotlin (`.kt`, `.kts`).
    Kotlin,
    /// Scala (`.scala`, `.sc`).
    Scala,
    /// Zig (`.zig`).
    Zig,
    /// Haskell (`.hs`).
    Haskell,
    /// Lua (`.lua`).
    Lua,
    /// Elixir (`.ex`, `.exs`).
    Elixir,
    /// POSIX / Bash shell (`.sh`, `.bash`, `.zsh`).
    Shell,
}

impl Language {
    /// Detect a language from a file's lowercase extension (no leading dot).
    ///
    /// Returns `None` for extensions the heuristic scanner does not cover.
    #[must_use]
    pub fn from_extension(ext: &str) -> Option<Self> {
        // Lowercase once so callers can pass mixed-case extensions.
        let lower = ext.to_ascii_lowercase();
        let lang = match lower.as_str() {
            "rs" => Self::Rust,
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Self::TypeScript,
            "py" | "pyi" => Self::Python,
            "go" => Self::Go,
            "java" => Self::Java,
            "c" | "h" => Self::C,
            "cc" | "cpp" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" => Self::Cpp,
            "cs" => Self::CSharp,
            "rb" => Self::Ruby,
            "php" | "phtml" => Self::Php,
            "swift" => Self::Swift,
            "kt" | "kts" => Self::Kotlin,
            "scala" | "sc" => Self::Scala,
            "zig" => Self::Zig,
            "hs" => Self::Haskell,
            "lua" => Self::Lua,
            "ex" | "exs" => Self::Elixir,
            "sh" | "bash" | "zsh" | "ksh" => Self::Shell,
            _ => return None,
        };
        Some(lang)
    }

    /// Detect a language from a file path by inspecting its extension.
    ///
    /// Splits on the final `.` of the final path component (so `foo.test.ts`
    /// keys off `ts`). Returns `None` when there is no extension or the
    /// extension is unrecognized.
    #[must_use]
    pub fn from_path(path: &str) -> Option<Self> {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        // A leading-dot file (`.gitignore`) has no real extension.
        let (_stem, ext) = name.rsplit_once('.')?;
        if ext.is_empty() {
            return None;
        }
        Self::from_extension(ext)
    }
}

/// Extract the names of top-level definitions from `source` using cheap,
/// per-language leader heuristics.
///
/// This is the dependency-free counterpart to tree-sitter extraction: it scans
/// line by line, strips comments/leading noise, and for each recognized
/// definition leader (`fn`, `def`, `class`, `struct`, `func`, `fun`, …) pulls
/// out the following identifier. The result feeds [`FileSymbols::defines`] so
/// the ranker can build a def→ref graph without compiling any grammar.
///
/// It is *heuristic*: it favours recall over precision, may miss exotic
/// declarations, and does not attempt to resolve scopes. Names are returned in
/// source order with duplicates removed (first occurrence wins).
#[must_use]
pub fn scan_definitions(lang: Language, source: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for raw in source.lines() {
        let line = strip_line_comment(lang, raw).trim();
        if line.is_empty() {
            continue;
        }
        for name in definitions_in_line(lang, line) {
            if seen.insert(name.clone()) {
                out.push(name);
            }
        }
    }
    out
}

/// Convenience wrapper: detect the language from `path`, then scan `source`.
///
/// Returns `None` when the path's extension is not one the scanner recognizes,
/// letting callers skip non-source files cheaply.
#[must_use]
pub fn scan_path(path: &str, source: &str) -> Option<(Language, Vec<String>)> {
    let lang = Language::from_path(path)?;
    Some((lang, scan_definitions(lang, source)))
}

/// Strip a trailing line comment so leaders inside comments are ignored.
///
/// Uses the dominant single-line comment token per language family. Block
/// comments and string-embedded comment tokens are intentionally *not* handled —
/// the scanner trades a little precision for staying allocation-light.
fn strip_line_comment(lang: Language, line: &str) -> &str {
    let token = match lang {
        // `#` line comments.
        Language::Python | Language::Ruby | Language::Elixir | Language::Shell => "#",
        // `--` line comments.
        Language::Haskell | Language::Lua => "--",
        // `//` line comments (C-family and friends, including Zig).
        Language::Rust
        | Language::TypeScript
        | Language::Go
        | Language::Java
        | Language::C
        | Language::Cpp
        | Language::CSharp
        | Language::Php
        | Language::Swift
        | Language::Kotlin
        | Language::Scala
        | Language::Zig => "//",
    };
    split_before(line, token)
}

/// Return the slice of `line` before the first occurrence of `token`.
fn split_before<'a>(line: &'a str, token: &str) -> &'a str {
    line.find(token).map_or(line, |idx| &line[..idx])
}

/// Extract any definition names declared on a single (already comment-stripped,
/// trimmed) line for `lang`. Most lines yield zero or one name; a few patterns
/// (e.g. Zig `const X = struct`) are handled specially.
fn definitions_in_line(lang: Language, line: &str) -> Vec<String> {
    match lang {
        Language::Rust => leader_defs(
            line,
            &[
                "fn",
                "struct",
                "enum",
                "trait",
                "type",
                "mod",
                "const",
                "static",
                "union",
                "macro_rules!",
            ],
        ),
        Language::TypeScript => ts_defs(line),
        Language::Python => leader_defs(line, &["def", "class"]),
        Language::Go => go_defs(line),
        Language::Java | Language::CSharp => brace_lang_defs(line, lang),
        Language::C | Language::Cpp => c_family_defs(line, lang),
        Language::Ruby => leader_defs(line, &["def", "class", "module"]),
        Language::Php => leader_defs(line, &["function", "class", "interface", "trait", "enum"]),
        Language::Swift => leader_defs(
            line,
            &[
                "func",
                "class",
                "struct",
                "enum",
                "protocol",
                "extension",
                "actor",
            ],
        ),
        Language::Kotlin => kotlin_defs(line),
        Language::Scala => leader_defs(line, &["def", "class", "object", "trait", "case class"]),
        Language::Zig => zig_defs(line),
        Language::Haskell => haskell_defs(line),
        Language::Lua => lua_defs(line),
        Language::Elixir => leader_defs(
            line,
            &["def", "defp", "defmodule", "defmacro", "defstruct", "defprotocol"],
        ),
        Language::Shell => shell_defs(line),
    }
}

/// Generic "leader keyword then identifier" extractor.
///
/// For each leader in `leaders`, if `line` begins with that leader followed by
/// whitespace (after skipping common modifier words like `pub`, `async`,
/// `export`, visibility keywords, etc.), the next identifier token is taken as
/// the defined name. Returns the single name found, or empty.
fn leader_defs(line: &str, leaders: &[&str]) -> Vec<String> {
    let rest = strip_modifiers(line);
    for leader in leaders {
        if let Some(after) = match_leader(rest, leader) {
            if let Some(name) = definition_name(after) {
                return vec![name];
            }
        }
    }
    Vec::new()
}

/// Construct keywords that can directly follow another leader/modifier and must
/// not themselves be mistaken for the declared name. Handles forms like Kotlin
/// `data class Foo` / `enum class Bar` / `annotation class Baz` and Scala
/// `case class Qux` where two keywords precede the real identifier.
const CONSTRUCT_KEYWORDS: &[&str] = &[
    "class",
    "struct",
    "enum",
    "interface",
    "object",
    "trait",
    "fn",
    "func",
    "fun",
    "def",
    "type",
    "record",
    "union",
    "namespace",
    "protocol",
    "actor",
];

/// Extract the declared name after a leader, skipping a single immediately
/// following construct keyword (so `enum class Foo` yields `Foo`, not `class`).
fn definition_name(after: &str) -> Option<String> {
    let trimmed = after.trim_start();
    for kw in CONSTRUCT_KEYWORDS {
        if let Some(rest) = match_leader(trimmed, kw) {
            // Only skip when a real identifier follows the second keyword.
            if let Some(name) = first_identifier(rest) {
                return Some(name);
            }
        }
    }
    first_identifier(after)
}

/// Strip leading visibility / modifier keywords shared across many languages so
/// the leader match can see the real declaration keyword.
///
/// Keywords that *also* serve as definition leaders (`const`, `static`, `data`)
/// are deliberately omitted here so they survive for the leader match — e.g.
/// Rust `const FOO` and Haskell `data Foo` must reach their leaders intact.
fn strip_modifiers(line: &str) -> &str {
    const MODIFIERS: &[&str] = &[
        "pub",
        "public",
        "private",
        "protected",
        "internal",
        "export",
        "default",
        "final",
        "abstract",
        "sealed",
        "open",
        "override",
        "async",
        "inline",
        "extern",
        "unsafe",
        "mut",
        "virtual",
        "partial",
        "suspend",
        "lateinit",
        "readonly",
        "declare",
        "implicit",
        "lazy",
        "annotation",
    ];
    let mut cur = line;
    loop {
        let trimmed = cur.trim_start();
        // Handle Rust `pub(crate)` / `pub(super)` visibility scopes.
        if let Some(after) = trimmed.strip_prefix("pub(") {
            if let Some(close) = after.find(')') {
                cur = &after[close + 1..];
                continue;
            }
        }
        let mut advanced = false;
        for m in MODIFIERS {
            if let Some(after) = match_leader(trimmed, m) {
                cur = after;
                advanced = true;
                break;
            }
        }
        if !advanced {
            return trimmed;
        }
    }
}

/// If `text` starts with `leader` as a whole word, return the remainder after
/// it (leading whitespace not yet trimmed). Whole-word means the char after the
/// leader is not an identifier char — so `fn` matches `fn foo` but not `fnord`.
/// A leader ending in `!` (e.g. `macro_rules!`) matches literally.
fn match_leader<'a>(text: &'a str, leader: &str) -> Option<&'a str> {
    let after = text.strip_prefix(leader)?;
    // For `name!`-style leaders the `!` already bounds the word.
    if leader.ends_with('!') {
        return Some(after);
    }
    match after.chars().next() {
        // Boundary: must be whitespace or a delimiter, not another ident char.
        Some(c) if is_ident_char(c) => None,
        _ => Some(after),
    }
}

/// Read the first identifier token from `text`, skipping leading whitespace and
/// any non-identifier punctuation directly before it (e.g. `*`, `&`, `(`).
fn first_identifier(text: &str) -> Option<String> {
    let mut chars = text.char_indices().peekable();
    // Skip until we hit a valid identifier-start character.
    let start = loop {
        let (i, c) = *chars.peek()?;
        if is_ident_start(c) {
            break i;
        }
        chars.next();
    };
    let mut end = text.len();
    for (i, c) in text[start..].char_indices() {
        if !is_ident_char(c) {
            end = start + i;
            break;
        }
    }
    let ident = &text[start..end];
    if ident.is_empty() {
        None
    } else {
        Some(ident.to_owned())
    }
}

/// `true` for characters allowed to start an identifier across the supported
/// languages (ASCII letters, `_`; PHP/Kotlin/etc. also use `$` and `@`).
const fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$'
}

/// `true` for characters allowed within an identifier.
const fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// TypeScript / JavaScript: functions, classes, interfaces, enums, types, plus
/// `const NAME = (…) =>` / `const NAME = function` arrow/assignment forms.
fn ts_defs(line: &str) -> Vec<String> {
    if let names @ [_, ..] = leader_defs(
        line,
        &["function", "class", "interface", "enum", "type", "namespace"],
    )
    .as_slice()
    {
        return names.to_vec();
    }
    // `const foo = (...) => {}` / `let bar = function() {}` style definitions.
    let rest = strip_modifiers(line);
    for binder in ["const", "let", "var"] {
        if let Some(after) = match_leader(rest, binder) {
            if let Some(name) = first_identifier(after) {
                let tail = after.trim_start();
                // Only treat as a definition when it binds a function/arrow.
                if tail.contains("=>") || tail.contains("function") {
                    return vec![name];
                }
            }
        }
    }
    Vec::new()
}

/// Go: `func`, `type` (struct/interface/alias). Receiver methods `func (r T) M()`
/// are handled by skipping a parenthesized receiver before the method name.
fn go_defs(line: &str) -> Vec<String> {
    let rest = strip_modifiers(line);
    if let Some(after) = match_leader(rest, "func") {
        let after = after.trim_start();
        // Method form: `func (recv Type) Name(...)` — skip the receiver group.
        let after = after.strip_prefix('(').map_or(after, |inner| {
            inner.find(')').map_or(after, |close| &inner[close + 1..])
        });
        if let Some(name) = first_identifier(after) {
            return vec![name];
        }
    }
    leader_defs(line, &["type"])
}

/// Java / C#: `class`, `interface`, `enum`, `record`, `struct` (C#), plus a
/// best-effort method-signature heuristic (`ReturnType name(` at statement
/// start). The method heuristic only fires when the line ends in `(` args and
/// is not a control-flow keyword, to keep false positives low.
fn brace_lang_defs(line: &str, lang: Language) -> Vec<String> {
    let type_leaders: &[&str] = if lang == Language::CSharp {
        &[
            "class",
            "interface",
            "enum",
            "record",
            "struct",
            "namespace",
            "delegate",
        ]
    } else {
        &["class", "interface", "enum", "record"]
    };
    let names = leader_defs(line, type_leaders);
    if !names.is_empty() {
        return names;
    }
    method_signature(line)
}

/// Kotlin: `fun`/`class`/`object`/`interface`/`enum` plus the modifier-prefixed
/// class idioms (`data class`, `enum class`, `sealed class`, `value class`).
///
/// `data`/`value`/`inner` are Kotlin-specific *class modifiers* that the shared
/// [`strip_modifiers`] deliberately leaves alone (to protect Haskell `data`), so
/// they are peeled here before the generic leader match.
fn kotlin_defs(line: &str) -> Vec<String> {
    let rest = strip_modifiers(line);
    // Peel Kotlin class modifiers that the shared stripper intentionally skips.
    let mut cur = rest;
    loop {
        let trimmed = cur.trim_start();
        let mut advanced = false;
        for m in ["data", "value", "inner", "companion"] {
            if let Some(after) = match_leader(trimmed, m) {
                // Only peel when a class-construct keyword still follows.
                let tail = after.trim_start();
                if ["class", "object", "enum", "interface"]
                    .iter()
                    .any(|kw| match_leader(tail, kw).is_some())
                {
                    cur = after;
                    advanced = true;
                    break;
                }
            }
        }
        if !advanced {
            break;
        }
    }
    leader_defs(cur, &["fun", "class", "object", "interface", "enum"])
}

/// C / C++: `struct`/`class`/`enum`/`union`/`namespace` declarations and
/// free-function / method definitions via the shared [`method_signature`]
/// heuristic.
fn c_family_defs(line: &str, lang: Language) -> Vec<String> {
    let type_leaders: &[&str] = if lang == Language::Cpp {
        &["class", "struct", "enum", "union", "namespace"]
    } else {
        &["struct", "enum", "union"]
    };
    let names = leader_defs(line, type_leaders);
    if !names.is_empty() {
        return names;
    }
    method_signature(line)
}

/// C-family control-flow / operator keywords that can appear as `keyword(` and
/// must never be mistaken for a function definition.
const C_CONTROL_KEYWORDS: &[&str] = &[
    "if", "for", "while", "switch", "return", "catch", "sizeof", "do", "else", "case", "throw", "new",
    "delete", "and", "or", "not",
];

/// Heuristic for C-family function/method definitions: a line shaped like
/// `… name(args)` where `name` is the identifier immediately before the first
/// `(`, the line is not a control-flow statement or bare call, and the text
/// after the matching `)` opens a body `{` or is a prototype (`;`). A preceding
/// return-type / modifier token must exist, which rules out plain calls.
/// Returns at most one name.
fn method_signature(line: &str) -> Vec<String> {
    let Some(paren) = line.find('(') else {
        return Vec::new();
    };
    let head = line[..paren].trim_end();
    // The name is the last identifier-run before `(`.
    let name_start = head
        .char_indices()
        .rev()
        .take_while(|&(_, c)| is_ident_char(c))
        .last()
        .map(|(i, _)| i);
    let Some(start) = name_start else {
        return Vec::new();
    };
    let name = &head[start..];
    // Reject control-flow keywords masquerading as calls.
    if name.is_empty() || C_CONTROL_KEYWORDS.contains(&name) {
        return Vec::new();
    }
    // A preceding token (return type / modifier) must exist for a definition;
    // a bare `name(` with nothing before it is almost always a call. An `=` in
    // the head means an assignment/initializer — a call expression, not a decl.
    let lead = head[..start].trim();
    if lead.is_empty() || lead.contains('=') {
        return Vec::new();
    }
    // Inspect what follows the matching close-paren: a `{` body opener or a `;`
    // prototype terminator marks a definition; anything else (`.`, `,`, `=`,
    // chained calls) means this was an expression, not a declaration.
    let after_args = match_paren_tail(&line[paren..]);
    let tail = after_args.trim_start();
    // A trailing C++ specifier (`const`/`noexcept`/`override`) still precedes a
    // body or prototype, so treat those as definition headers too.
    let trailing_specifier = ["const", "noexcept", "override"]
        .iter()
        .any(|kw| tail.starts_with(kw));
    let looks_like_def = tail.starts_with('{')
        || tail.is_empty() // header continues on the next line (e.g. `void f(`)
        || tail.starts_with(';')
        || trailing_specifier;
    if !looks_like_def {
        return Vec::new();
    }
    vec![name.to_owned()]
}

/// Given a slice beginning at `(`, return the slice immediately after its
/// matching `)` (balanced). Returns the empty string when unbalanced (the
/// argument list spills onto later lines).
fn match_paren_tail(from_open: &str) -> &str {
    let mut depth: usize = 0;
    for (i, c) in from_open.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &from_open[i + 1..];
                }
            }
            _ => {}
        }
    }
    ""
}

/// Zig: `fn name(`, `pub fn name(`, and the `const Name = struct/enum/union {`
/// type-definition idiom.
fn zig_defs(line: &str) -> Vec<String> {
    let rest = strip_modifiers(line);
    if let Some(after) = match_leader(rest, "fn") {
        if let Some(name) = first_identifier(after) {
            return vec![name];
        }
    }
    // `const Name = struct {` / `= enum {` / `= union {` / `= opaque {`.
    for binder in ["const", "var"] {
        if let Some(after) = match_leader(rest, binder) {
            if let Some(name) = first_identifier(after) {
                let tail = after.trim_start();
                if tail.contains("struct")
                    || tail.contains("enum")
                    || tail.contains("union")
                    || tail.contains("opaque")
                {
                    return vec![name];
                }
            }
        }
    }
    Vec::new()
}

/// Haskell: top-level type signatures (`name :: …`), `data`/`newtype`/`type`
/// declarations, `class`/`instance` heads. Only column-0 declarations count as
/// top-level, which the caller approximates by trimming — so we additionally
/// require the original line to be unindented by checking the leading char.
fn haskell_defs(line: &str) -> Vec<String> {
    // `data Foo = …`, `newtype Bar = …`, `type Baz = …`, `class C a where`.
    if let names @ [_, ..] = leader_defs(line, &["data", "newtype", "type", "class"]).as_slice() {
        return names.to_vec();
    }
    // Top-level value/function signature: `name :: Type`.
    if let Some(idx) = line.find("::") {
        let head = line[..idx].trim();
        // A single identifier (or comma-separated group) to the left of `::`.
        let first = head.split(',').next().unwrap_or(head).trim();
        if let Some(name) = first_identifier(first) {
            if name == first || first.starts_with(&name) {
                return vec![name];
            }
        }
    }
    Vec::new()
}

/// Lua: `function name(`, `function tbl.name(`, `function tbl:method(`, and
/// `local function name(`. The dotted/colon-qualified tail name is taken.
fn lua_defs(line: &str) -> Vec<String> {
    let rest = strip_modifiers(line);
    let rest = match_leader(rest, "local").map_or(rest, |a| a.trim_start());
    if let Some(after) = match_leader(rest, "function") {
        let after = after.trim_start();
        // Qualified name `a.b:c` — take the final segment as the symbol.
        let qualified: String = after
            .chars()
            .take_while(|&c| is_ident_char(c) || c == '.' || c == ':')
            .collect();
        let leaf = qualified.rsplit(['.', ':']).next().unwrap_or(&qualified);
        if let Some(name) = first_identifier(leaf) {
            return vec![name];
        }
    }
    Vec::new()
}

/// Shell: POSIX `name() {` and ksh/bash `function name { … }` definitions.
fn shell_defs(line: &str) -> Vec<String> {
    // `function name` form.
    if let Some(after) = match_leader(line, "function") {
        if let Some(name) = first_identifier(after) {
            return vec![name];
        }
    }
    // `name()` / `name ()` POSIX form: identifier immediately followed by `()`.
    if let Some(paren) = line.find('(') {
        let head = line[..paren].trim();
        // Only a bare identifier may precede `(` in a function definition.
        if !head.is_empty() && head.chars().all(is_ident_char) {
            let after_paren = line[paren..].trim_start();
            if after_paren.starts_with("()") || after_paren.starts_with("( )") {
                return vec![head.to_owned()];
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    fn def(file: &str, defines: &[&str], tokens: u32) -> FileSymbols {
        FileSymbols::new(
            file,
            defines.iter().map(|s| (*s).to_string()).collect(),
            vec![],
            tokens,
        )
    }

    fn refs(file: &str, references: &[&str], tokens: u32) -> FileSymbols {
        FileSymbols::new(
            file,
            vec![],
            references.iter().map(|s| (*s).to_string()).collect(),
            tokens,
        )
    }

    #[test]
    fn empty_input_is_handled_without_panic() {
        assert!(pagerank(&[], 0.85, 10).is_empty());
        assert!(personalized_pagerank(&[], &["x".into()], 0.85, 10).is_empty());
        assert_eq!(build_map(&[], &[], 1_000), Err(RepoMapError::Empty));
    }

    #[test]
    fn widely_referenced_definer_scores_highest() {
        // core.rs defines Engine; three files reference it. core should win.
        let files = vec![
            def("core.rs", &["Engine"], 10),
            refs("a.rs", &["Engine"], 10),
            refs("b.rs", &["Engine"], 10),
            refs("c.rs", &["Engine"], 10),
        ];
        let ranked = pagerank(&files, 0.85, 30);
        assert_eq!(ranked[0].0, "core.rs");
        // The definer outranks each of its referrers.
        let core = ranked.iter().find(|(f, _)| f == "core.rs").unwrap().1;
        let a = ranked.iter().find(|(f, _)| f == "a.rs").unwrap().1;
        assert!(core > a, "definer {core} should beat referrer {a}");
    }

    #[test]
    fn scores_form_a_probability_distribution() {
        let files = vec![
            def("core.rs", &["Engine"], 10),
            refs("a.rs", &["Engine"], 10),
            refs("b.rs", &["Engine"], 10),
        ];
        let total: f64 = pagerank(&files, 0.85, 50).iter().map(|(_, s)| s).sum();
        assert!((total - 1.0).abs() < 1e-9, "scores must sum to 1, got {total}");
    }

    #[test]
    fn personalization_biases_toward_focus() {
        // Two symmetric definers, each referenced once. Without focus they tie;
        // focusing on b.rs must lift it above a.rs.
        let files = vec![
            FileSymbols::new("a.rs", vec!["A".into()], vec!["B".into()], 10),
            FileSymbols::new("b.rs", vec!["B".into()], vec!["A".into()], 10),
        ];
        let focus = vec!["b.rs".to_string()];
        let biased = personalized_pagerank(&files, &focus, 0.85, 40);
        let a = biased.iter().find(|(f, _)| f == "a.rs").unwrap().1;
        let b = biased.iter().find(|(f, _)| f == "b.rs").unwrap().1;
        assert!(b > a, "focus file b ({b}) should outrank a ({a})");
    }

    #[test]
    fn build_map_respects_token_budget() {
        let files = vec![
            def("core.rs", &["Engine"], 40),
            refs("a.rs", &["Engine"], 40),
            refs("b.rs", &["Engine"], 40),
        ];
        // Budget of 100 fits exactly two 40-token files (80 <= 100, 120 > 100).
        let map = build_map(&files, &[], 100).unwrap();
        let spent: u32 = map
            .iter()
            .map(|e| files.iter().find(|f| f.file == e.file).unwrap().tokens)
            .sum();
        assert!(spent <= 100, "spent {spent} must stay within budget");
        assert_eq!(map.len(), 2, "exactly two files fit");
    }

    #[test]
    fn build_map_skips_oversized_then_packs_smaller() {
        // Highest-ranked file is too big; a smaller lower-ranked one still fits.
        let files = vec![
            FileSymbols::new("big.rs", vec!["Hub".into()], vec![], 500),
            refs("u1.rs", &["Hub"], 10),
            refs("u2.rs", &["Hub"], 10),
        ];
        let map = build_map(&files, &[], 30).unwrap();
        assert!(
            !map.iter().any(|e| e.file == "big.rs"),
            "oversized file must be skipped"
        );
        assert!(map.iter().any(|e| e.file == "u1.rs"));
    }

    #[test]
    fn deterministic_tie_break_by_file_name() {
        // Three isolated files with no edges all share the same score; output
        // must be alphabetical and stable across runs.
        let files = vec![def("z.rs", &[], 10), def("m.rs", &[], 10), def("a.rs", &[], 10)];
        let first = pagerank(&files, 0.85, 10);
        let second = pagerank(&files, 0.85, 10);
        assert_eq!(first, second, "ranking must be deterministic");
        let order: Vec<&str> = first.iter().map(|(f, _)| f.as_str()).collect();
        assert_eq!(order, ["a.rs", "m.rs", "z.rs"]);
    }

    #[test]
    fn build_map_surfaces_defined_symbols() {
        let files = vec![
            def("core.rs", &["Engine", "Config"], 10),
            refs("a.rs", &["Engine"], 10),
        ];
        let map = build_map(&files, &[], 1_000).unwrap();
        let core = map.iter().find(|e| e.file == "core.rs").unwrap();
        assert_eq!(core.symbols, vec!["Engine".to_string(), "Config".to_string()]);
    }

    #[test]
    fn unknown_focus_falls_back_to_uniform() {
        let files = vec![def("core.rs", &["Engine"], 10), refs("a.rs", &["Engine"], 10)];
        let plain = pagerank(&files, 0.85, 30);
        let bogus = personalized_pagerank(&files, &["nonexistent.rs".into()], 0.85, 30);
        assert_eq!(plain, bogus, "unknown focus must degrade to plain pagerank");
    }

    // ---- lightweight definition scanner -------------------------------------

    /// Assert the scanner extracts exactly `expected` (order-sensitive) from
    /// `src` under `lang`.
    fn assert_defs(lang: Language, src: &str, expected: &[&str]) {
        let got = scan_definitions(lang, src);
        let want: Vec<String> = expected.iter().map(|s| (*s).to_string()).collect();
        assert_eq!(got, want, "scanning {lang:?}");
    }

    #[test]
    fn extension_and_path_detection() {
        assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
        assert_eq!(Language::from_extension("CPP"), Some(Language::Cpp));
        assert_eq!(Language::from_extension("exs"), Some(Language::Elixir));
        assert_eq!(Language::from_extension("kts"), Some(Language::Kotlin));
        assert_eq!(Language::from_extension("nope"), None);
        assert_eq!(Language::from_path("src/main.rs"), Some(Language::Rust));
        assert_eq!(Language::from_path(r"C:\proj\a\b.swift"), Some(Language::Swift));
        assert_eq!(Language::from_path("foo.test.ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_path("Makefile"), None);
        assert_eq!(Language::from_path(".gitignore"), None);
    }

    #[test]
    fn existing_languages_still_scan() {
        assert_defs(
            Language::Rust,
            "pub fn build() {}\nstruct Engine;\nconst MAX: u8 = 9;\nmod inner {}",
            &["build", "Engine", "MAX", "inner"],
        );
        assert_defs(
            Language::Python,
            "def run(x):\n    pass\nclass Widget:\n    pass",
            &["run", "Widget"],
        );
        assert_defs(
            Language::Go,
            "func Hello() {}\nfunc (r *Repo) Save() {}\ntype Repo struct {}",
            &["Hello", "Save", "Repo"],
        );
        assert_defs(
            Language::TypeScript,
            "export function load() {}\nclass App {}\nconst handler = () => {};",
            &["load", "App", "handler"],
        );
        assert_defs(
            Language::Java,
            "public class Foo {\n    public int add(int a) { return a; }\n}",
            &["Foo", "add"],
        );
    }

    #[test]
    fn scans_c_and_cpp() {
        assert_defs(
            Language::C,
            "int add(int a, int b) {\n    return a + b;\n}\nstruct Point { int x; };",
            &["add", "Point"],
        );
        assert_defs(
            Language::Cpp,
            "class Widget {\npublic:\n    void draw();\n};\nnamespace gfx {}",
            // A method prototype `void draw();` is a legitimate declaration.
            &["Widget", "draw", "gfx"],
        );
        // A bare call must NOT be picked up as a definition.
        assert_defs(Language::C, "    do_thing(x, y);", &[]);
        // Control-flow keywords must not be treated as functions.
        assert_defs(Language::C, "    if (cond) {", &[]);
    }

    #[test]
    fn scans_csharp_ruby_php() {
        assert_defs(
            Language::CSharp,
            "public record Money(decimal Amount);\ninterface IRepo { }",
            &["Money", "IRepo"],
        );
        assert_defs(
            Language::Ruby,
            "module M\n  class Cat\n    def meow\n    end\n  end\nend",
            &["M", "Cat", "meow"],
        );
        assert_defs(
            Language::Php,
            "<?php\nclass User {\n    public function name() {}\n}",
            &["User", "name"],
        );
    }

    #[test]
    fn scans_swift_kotlin_scala() {
        assert_defs(
            Language::Swift,
            "struct Vec {}\nfunc dot() {}\nprotocol Drawable {}\nenum Color {}",
            &["Vec", "dot", "Drawable", "Color"],
        );
        assert_defs(
            Language::Kotlin,
            "fun main() {}\ndata class Point(val x: Int)\nenum class Dir { N }\nobject Reg {}",
            &["main", "Point", "Dir", "Reg"],
        );
        assert_defs(
            Language::Scala,
            "object App {}\ncase class Item(id: Int)\ntrait Show {}\ndef run() = {}",
            &["App", "Item", "Show", "run"],
        );
    }

    #[test]
    fn scans_zig_haskell_lua_elixir_shell() {
        assert_defs(
            Language::Zig,
            "pub fn add(a: u8) u8 {\n    return a;\n}\nconst Point = struct {\n    x: u8,\n};",
            &["add", "Point"],
        );
        assert_defs(
            Language::Haskell,
            "module M where\nfib :: Int -> Int\nfib n = n\ndata Tree = Leaf\nnewtype Age = Age Int",
            &["fib", "Tree", "Age"],
        );
        assert_defs(
            Language::Lua,
            "function greet(name)\nend\nlocal function helper()\nend\nfunction M.attach(o)\nend",
            &["greet", "helper", "attach"],
        );
        assert_defs(
            Language::Elixir,
            "defmodule Calc do\n  def add(a, b), do: a + b\n  defp secret(), do: 1\nend",
            &["Calc", "add", "secret"],
        );
        assert_defs(
            Language::Shell,
            "build() {\n  echo hi\n}\nfunction deploy {\n  echo bye\n}",
            &["build", "deploy"],
        );
    }

    #[test]
    fn comments_are_ignored() {
        // A `fn` mentioned only inside a comment must not yield a symbol.
        assert_defs(Language::Rust, "// fn ghost() {}\nfn real() {}", &["real"]);
        assert_defs(
            Language::Python,
            "# def ghost():\ndef real():\n    pass",
            &["real"],
        );
        assert_defs(
            Language::Lua,
            "-- function ghost()\nfunction real()\nend",
            &["real"],
        );
    }

    #[test]
    fn scan_path_detects_then_scans() {
        let (lang, defs) = scan_path("lib/widget.rb", "class Widget\n  def go\n  end\nend").unwrap();
        assert_eq!(lang, Language::Ruby);
        assert_eq!(defs, vec!["Widget".to_string(), "go".to_string()]);
        assert!(scan_path("README.md", "# hi").is_none());
    }

    #[test]
    fn scanner_output_feeds_the_ranker() {
        // End-to-end: scanned definitions populate FileSymbols and rank sensibly.
        let core_src = "pub struct Engine {}\npub fn start() {}";
        let (lang, core_defs) = scan_path("core.rs", core_src).unwrap();
        assert_eq!(lang, Language::Rust);
        let files = vec![
            FileSymbols::new("core.rs", core_defs, vec![], 20),
            refs("a.rs", &["Engine"], 10),
            refs("b.rs", &["Engine"], 10),
        ];
        let ranked = pagerank(&files, 0.85, 30);
        assert_eq!(ranked[0].0, "core.rs", "the scanned definer should rank first");
    }

    #[test]
    fn scan_dedups_repeated_names() {
        // The same symbol declared twice is reported once (first occurrence).
        let defs = scan_definitions(Language::Rust, "fn dup() {}\nfn dup() {}");
        assert_eq!(defs, vec!["dup".to_string()]);
    }

    // ---- multi-root merge + re-rank -----------------------------------------

    #[test]
    fn merge_and_rerank_over_three_roots_is_deterministic() {
        // Three roots, each contributing files that reference a shared definer.
        let roots = || {
            vec![
                vec![def("core.rs", &["Engine"], 10), refs("a.rs", &["Engine"], 10)],
                vec![refs("b.rs", &["Engine"], 10), def("util.rs", &["Helper"], 10)],
                vec![refs("c.rs", &["Engine", "Helper"], 10)],
            ]
        };
        let first = merge_and_rerank_maps(roots(), &[], 1_000).unwrap();
        let second = merge_and_rerank_maps(roots(), &[], 1_000).unwrap();
        assert_eq!(first, second, "multi-root ranking must be deterministic");
    }

    #[test]
    fn merge_dedups_file_present_in_two_roots() {
        // core.rs appears in two roots; the merged map must contain it once.
        let roots = vec![
            vec![def("core.rs", &["Engine"], 10), refs("a.rs", &["Engine"], 10)],
            vec![def("core.rs", &["Engine"], 10), refs("b.rs", &["Engine"], 10)],
        ];
        let map = merge_and_rerank_maps(roots, &[], 10_000).unwrap();
        let core_count = map.iter().filter(|e| e.file == "core.rs").count();
        assert_eq!(core_count, 1, "duplicate path must collapse to one entry");
    }

    #[test]
    fn merge_empty_input_is_empty_error() {
        // No roots at all, and roots that are all empty, both map to Empty.
        assert_eq!(
            merge_and_rerank_maps(vec![], &[], 1_000),
            Err(RepoMapError::Empty)
        );
        assert_eq!(
            merge_and_rerank_maps(vec![vec![], vec![]], &[], 1_000),
            Err(RepoMapError::Empty)
        );
        assert_eq!(build_map_multi_root(vec![], &[], 1_000), Err(RepoMapError::Empty));
    }

    #[test]
    fn single_root_merge_equals_build_map() {
        // Wrapping one root's files must yield exactly what build_map produces.
        let files = vec![
            def("core.rs", &["Engine", "Config"], 20),
            refs("a.rs", &["Engine"], 10),
            refs("b.rs", &["Engine"], 10),
        ];
        let direct = build_map(&files, &[], 100).unwrap();
        let via_merge = merge_and_rerank_maps(vec![files], &[], 100).unwrap();
        assert_eq!(via_merge, direct, "single-root path must be byte-identical");
    }

    #[test]
    fn build_map_multi_root_delegates_to_merge() {
        // The public multi-root entry point matches the merge helper directly.
        let roots = vec![
            vec![def("core.rs", &["Engine"], 10)],
            vec![refs("a.rs", &["Engine"], 10), refs("b.rs", &["Engine"], 10)],
        ];
        let via_helper = merge_and_rerank_maps(roots.clone(), &[], 1_000).unwrap();
        let via_entry = build_map_multi_root(roots, &[], 1_000).unwrap();
        assert_eq!(via_entry, via_helper);
    }

    #[test]
    fn per_root_ranks_each_root_independently() {
        let roots = vec![
            vec![def("a/core.rs", &["A"], 10), refs("a/use.rs", &["A"], 10)],
            vec![def("b/core.rs", &["B"], 10)],
        ];
        let maps = build_map_per_root(&roots, &[], 1_000).unwrap();
        assert_eq!(maps.len(), 2);
        assert_eq!(maps[0].root_index, 0);
        assert_eq!(maps[1].root_index, 1);
        // Root 1's map contains ONLY root 1's file — each root is its own graph.
        let r1: Vec<&str> = maps[1].entries.iter().map(|e| e.file.as_str()).collect();
        assert_eq!(r1, vec!["b/core.rs"]);
        let mut r0: Vec<&str> = maps[0].entries.iter().map(|e| e.file.as_str()).collect();
        r0.sort_unstable();
        assert_eq!(r0, vec!["a/core.rs", "a/use.rs"]);
    }

    #[test]
    fn per_root_skips_empty_roots_and_errors_when_all_empty() {
        let roots = vec![vec![], vec![def("x.rs", &["X"], 5)], vec![]];
        let maps = build_map_per_root(&roots, &[], 1_000).unwrap();
        assert_eq!(maps.len(), 1);
        assert_eq!(maps[0].root_index, 1, "preserves the original root index");
        assert_eq!(
            build_map_per_root(&[vec![], vec![]], &[], 1_000),
            Err(RepoMapError::Empty)
        );
    }

    #[test]
    fn per_root_splits_budget_across_roots() {
        // Two roots, each a single 10-token file; a 20-token total splits to 10
        // per root so each admits its file — a per-root fair share regardless of
        // the other root's size.
        let roots = vec![vec![def("a.rs", &["A"], 10)], vec![def("b.rs", &["B"], 10)]];
        let maps = build_map_per_root(&roots, &[], 20).unwrap();
        assert_eq!(maps.len(), 2);
        assert_eq!(maps[0].entries.len(), 1);
        assert_eq!(maps[1].entries.len(), 1);
    }
}
