# origin-codegraph

> Native code knowledge graph with tree-sitter extraction and SQLite index.

## Purpose

`origin-codegraph` builds a queryable knowledge graph of a codebase. tree-sitter
parses source into nodes (functions, types, modules) and edges (calls,
implements, …); these land as small SQLite rows pointing at CAS-stored
signature/body/evidence blobs. A typed query DSL (no NL, no in-tool LLM hop)
walks the graph for paths, neighbours, communities, god-nodes, and recent
changes. An incremental rebuild driver re-extracts only changed paths.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Language`, `Parser`, `LangError` | enum/struct | tree-sitter language selection + parser. |
| `extract_nodes` / `extract_nodes_with_cas` / `extract_edges` | fn | AST → `CodeNode`/`CodeEdge`. |
| `CodeNode`, `CodeEdge`, `NodeKind`, `EdgeKind` | struct/enum | Extracted graph entities. |
| `CodeGraphIndex`, `EntityId`, `NodeRow`, `EdgeRow` | struct | SQLite + CAS index. |
| `CodeNodeRecord`, `Confidence` | struct/enum | Insert shape + provenance level. |
| `query::Query` / `QueryResult` / `dispatch` | enum/fn | Typed graph queries. |
| `rebuild::rebuild_paths` / `RebuildReport` | fn/struct | Incremental rebuild driver. |
| `sidecar::Sidecar` (+ `NoopSidecar`, `LopdfTextSidecar`) | trait | Pluggable extra extractors. |
| `ask::classify` / `Route` / `MemRouter` | fn/enum/trait | Lexical query router. |

## Key types

```rust
pub struct EntityId(pub [u8; 32]); // blake3(kind, name, file_path, range_start)

pub enum NodeKind { Function, Method, Struct, Class, Trait, Interface, Module }
pub enum EdgeKind { Calls, Mentions, Implements, Extends }

pub enum Query {
    Path { from: EntityId, to: EntityId, max_hops: usize },
    Neighbors { node: EntityId, depth: usize },
    Communities,
    GodNodes { top_per_partition: usize },
    RecentChanges { since_ms: i64 },
}

pub enum QueryResult { Nodes(Vec<NodeRow>), Path(Vec<NodeRow>), Partitions(Vec<Vec<NodeRow>>), Empty }

pub fn dispatch(idx: &CodeGraphIndex, q: Query) -> Result<QueryResult, QueryError>;
```

## How it works

```text
source bytes ─tree-sitter→ CodeNode/CodeEdge ─insert→ CodeGraphIndex
                                                   │
              SQLite rows (kind,name,lang,path,range,sig/body CAS handles)
              CAS blobs (signature, body, evidence — content-deduplicated)
                                                   │
                                  dispatch(Query) walks edges/SQL
```

`insert_node` computes a deterministic `EntityId` from `(kind, name, file_path,
range_start)`, so the same span in the same file upserts the same row. Insert is
content-deduplicating: identical signature bytes from two files collapse to one
CAS handle, which is indexed so `nodes_by_signature` can fan out from a signature
to every declaring file.

The query dispatcher uses the index's own SQL/edge primitives. `Neighbors` is a
BFS over `edges_from`; `Path` is bounded by `max_hops`; `Communities` runs Label
Propagation (O(E) per sweep, ~5 sweeps) directly over the edge table and hashes
the edge list with blake3 for future caching; `GodNodes` ranks each community's
members by in-degree. `rebuild_paths` re-extracts changed paths into a
`RebuildReport`. The `Sidecar` trait lets extra extractors run alongside
tree-sitter (`LopdfTextSidecar` emits one `Confidence::Extracted` entity per PDF
page; `NoopSidecar` is the default). `ask::classify` is a lexical-only router
(two precompiled regexes, one truth-table) feeding the `MemRouter` trait.

## Dependencies & features

- `tree-sitter` (`=0.22.6`) plus pinned grammars for Rust, TypeScript, Python,
  Go, Java, C/C++, C#, Ruby, Bash, PHP, Swift, Kotlin, Scala, Haskell, Elixir,
  Lua — each pinned to a generation exposing the classic `language()` ABI
  compatible with the 0.22.6 core.
- `rusqlite` (`bundled`), `rkyv` (validation, for `Confidence`/evidence blobs),
  `blake3`, `fastcdc` (AST-biased chunker), `lopdf` (PDF sidecar), `regex`,
  `thiserror`. Workspace crates `origin-cas` + `origin-store`.
- Dev-deps: `criterion` (`incremental` bench), `tempfile`. No optional features.

## Used by

`crates/*/Cargo.toml` matches for `origin-codegraph`:

- `crates/origin-codegraph/Cargo.toml`
- `crates/origin-daemon/Cargo.toml`
- `crates/origin-tools/Cargo.toml`

## Testing

Integration tests under `crates/origin-codegraph/tests/` mirror the modules:
`extract.rs`, `index.rs`, `query.rs`, `rebuild.rs`, `lang.rs`, `chunker.rs`,
`ask.rs`, `sidecar.rs`. The `incremental` Criterion benchmark exercises the
rebuild path.

## See also

- [Memory & code graph subsystem](../subsystems/memory-and-codegraph.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
