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
    pub fn new(
        file: impl Into<String>,
        defines: Vec<String>,
        references: Vec<String>,
        tokens: u32,
    ) -> Self {
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
    let defines: HashMap<&str, &Vec<String>> =
        files.iter().map(|f| (f.file.as_str(), &f.defines)).collect();
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
        let symbols = defines
            .get(file.as_str())
            .map_or_else(Vec::new, |d| (*d).clone());
        out.push(RankedEntry {
            file,
            score,
            symbols,
        });
    }
    Ok(out)
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
fn power_iterate(
    out_links: &[Vec<usize>],
    teleport: &[f64],
    damping: f64,
    iters: u32,
    n: usize,
) -> Vec<f64> {
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
        let files = vec![
            def("core.rs", &["Engine"], 10),
            refs("a.rs", &["Engine"], 10),
        ];
        let plain = pagerank(&files, 0.85, 30);
        let bogus = personalized_pagerank(&files, &["nonexistent.rs".into()], 0.85, 30);
        assert_eq!(plain, bogus, "unknown focus must degrade to plain pagerank");
    }
}
