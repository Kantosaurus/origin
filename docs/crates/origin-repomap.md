# origin-repomap

> Personalized-PageRank repo map over a symbol graph, packed into a token budget.

## Purpose

`origin-repomap` is the *ranker* that turns a code symbol graph (which file
defines and which references each symbol) into the most context-worthy slice of
a repository, packed to fit a token budget — the "repo map" trick popularized by
`aider`. It runs personalized PageRank over a directed file graph, then greedily
admits top-ranked files until the budget is exhausted. The crate is pure (no I/O,
no async, no tree-sitter), so it is deterministic; a built-in heuristic scanner
extracts definition names when the upstream `origin-codegraph` graph is absent.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `FileSymbols` | struct | Per-file row: `defines`, `references`, `tokens`. |
| `RankedEntry` | struct | One ranked file: `file`, `score`, `symbols`. |
| `pagerank` | fn | Unbiased PageRank over the def→ref graph. |
| `personalized_pagerank` | fn | PageRank with teleport biased to `focus`. |
| `build_map` | fn | Rank + greedily pack into a token budget. |
| `merge_and_rerank_maps` / `build_map_multi_root` | fn | Merge roots, rank globally. |
| `build_map_per_root` / `RootMap` | fn/struct | Rank each root independently. |
| `Language`, `scan_definitions`, `scan_path` | enum/fn | Heuristic definition scanner. |
| `RepoMapError` | enum | `Empty`. |

## Key types

```rust
pub struct FileSymbols {
    pub file: String,            // node identity + tie-break key
    pub defines: Vec<String>,
    pub references: Vec<String>,
    pub tokens: u32,             // approx cost of including this file in the map
}

pub struct RankedEntry {
    pub file: String,
    pub score: f64,              // PageRank score (higher = more central)
    pub symbols: Vec<String>,
}

pub fn build_map(files: &[FileSymbols], focus: &[String], token_budget: u32)
    -> Result<Vec<RankedEntry>, RepoMapError>;

pub fn personalized_pagerank(files: &[FileSymbols], focus: &[String],
    damping: f64, iters: u32) -> Vec<(String, f64)>;
```

## How it works

```text
FileSymbols rows
   edge A → B  whenever A references a symbol B defines (self-refs ignored)
   personalized PageRank (damping 0.85, 24 iters), teleport biased to `focus`
   → (file, score) sorted desc, ties by file name
build_map: greedily admit ranked files while spent + tokens ≤ budget
```

Importance flows toward files that *define* widely-used symbols, so core types
and hot utilities bubble to the top; a `focus` set concentrates the random-restart
vector on the files the user is actively editing (focus entries naming unknown
files are ignored, degrading to plain PageRank). Packing is greedy but
non-starving: a file that overflows the remaining budget is skipped while
scanning continues, so a smaller lower-ranked file can still fit. Defaults are
`DEFAULT_DAMPING = 0.85` and `DEFAULT_ITERS = 24`.

Multi-root support comes in two flavours: `merge_and_rerank_maps` /
`build_map_multi_root` concatenate every root's rows (dedup by path, first wins),
then rank globally so cross-root dependencies count; `build_map_per_root` ranks
each root independently under its even share of the budget, returning one
`RootMap` per non-empty root so a small root is never buried by a large one.

The heuristic scanner (`scan_definitions` / `scan_path`) detects definition names
per `Language` (Rust, TypeScript/JS, Python, Go, Java, C, C++, C#, Ruby, PHP,
Swift, Kotlin, Scala, Zig, Haskell, Lua, Elixir, Shell) without compiling any
grammar — it favours recall, strips line comments, and de-duplicates names in
source order.

## Dependencies & features

- `serde` (`FileSymbols`/`RankedEntry`/`RootMap` are serializable) and
  `thiserror`. No async, no I/O, no tree-sitter. No optional cargo features.

## Used by

`crates/*/Cargo.toml` matches for `origin-repomap`:

- `crates/origin-daemon/Cargo.toml`
- `crates/origin-repomap/Cargo.toml`

## Testing

All tests are in-file in `lib.rs` (the crate ships one large `lib.rs` with its
unit tests). They cover PageRank ordering, focus biasing, greedy budget packing
(including the non-starving skip), multi-root merge/dedup and per-root splitting,
and the per-language definition scanner.

## See also

- [Memory & code graph subsystem](../subsystems/memory-and-codegraph.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
