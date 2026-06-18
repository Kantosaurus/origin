# origin-mem

> Conversation memory with ONNX MiniLM embeddings, HNSW search, and temporal-decay re-ranking.

## Purpose

`origin-mem` is the conversation-memory subsystem: it embeds text with an ONNX
MiniLM model, int8-quantizes the vectors, indexes them in an HNSW graph, and
re-ranks search hits by a temporal-decay formula. Memory bodies live in CAS
(`origin-cas`) and metadata/edges in SQLite (`origin-store`). Around that sit a
turn-end proposer, a context injector for the system prompt, and an idle
consolidator that supersedes/merges similar memories.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Embedder`, `EMBED_DIM` | struct/const | ONNX MiniLM → `Vec<f32>` (dim 384). |
| `Quantizer`, `EncodedVector`, `NUM_CENTROIDS` | struct | Int8 k-means residual codec. |
| `MemIndex`, `Candidate`, `SearchOpts`, `MetaRow` | struct | HNSW + decay re-rank. |
| `MemoryStore`, `MemoryRecord`, `MemoryId`, `EdgeKind` | struct/enum | SQLite + CAS store. |
| `Proposer`, `MemoryProposal` | struct | Turn-end extraction of memorable spans. |
| `Injector`, `InjectedContext`, `memory_id_to_u64` | struct/fn | `<context>` block builder. |
| `Consolidator`, `ConsolidationReport` | struct | Idle clustering + supersede pass. |
| `SECS_PER_DAY`, `MS_PER_DAY` | const | Recency-scoring time constants. |

## Key types

```rust
pub const EMBED_DIM: usize = 384;
pub const NUM_CENTROIDS: usize = 256;

pub struct Candidate {
    pub id: u64,
    pub raw_sim: f32,         // 1 - DistDot distance
    pub age_days: f32,
    pub cluster_priority: f32,
    pub edge_boost: f32,
    pub score: f32,           // final re-ranked score
}

pub struct SearchOpts {       // Default: 3000 / 5 / 30.0 / true
    pub shortlist_k: usize,
    pub top_n: usize,
    pub decay_tau_days: f32,
    pub drop_superseded: bool,
}

pub struct MemoryRecord {
    pub id: MemoryId,                 // = ulid::Ulid
    pub encoded: EncodedVector,
    pub body_handle: [u8; 32],        // CAS blake3 hash
    pub body_preview: String,
    pub tags: Vec<String>,
    pub created_at_ms: i64,
    pub last_seen_at_ms: i64,
    pub superseded_by: Option<MemoryId>,
    pub cluster_priority: f32,
}
```

## How it works

```text
text ─Embedder(ONNX MiniLM)→ [f32;384] ─Quantizer→ EncodedVector
                                   │                     │
                                   ▼                     ▼
                              MemIndex(HNSW)        MemoryStore
                                   │            (SQLite rows + CAS bodies)
                          search → shortlist_k ──► re-rank ──► top_n Candidates
```

`MemIndex` wraps `hnsw_rs::Hnsw` with a `DistDot` metric (returns `1 − dot`, so
L2-normalised vectors give `1 − cos_sim`). Public `u64` ids are used directly as
HNSW data-ids. After fetching `shortlist_k` neighbours it re-ranks each via a
caller-supplied `lookup(id) -> MetaRow` closure with the formula

```text
score = raw_sim · exp(-age_days / decay_tau_days) · cluster_priority · (1 + edge_boost)
```

returning the top `top_n` (descending score, ties by id ascending), dropping
superseded rows when requested.

The `Quantizer` stores each vector as `(centroid_id, i8 deltas)` against 256
k-means centroids and a global scale — ~32× smaller than raw f32, with an
asymmetric dot-product query path; it serializes to a tagged little-endian byte
buffer persisted in the `mem_quantizer` singleton SQLite row. `MemoryStore`
keeps bodies in CAS (refcounted), tags as a 128-bit bitset over a dictionary
table, and supports `save`/`get`/`forget`/`add_edge`/`mark_superseded`/
`bump_priority`. The `Proposer` uses a `regex::RegexSet` single-pass scan to
emit `MemoryProposal`s at turn end; the `Injector` embeds the prompt, searches,
and formats hits into a `<context>` block for the system-prompt Sticky band; the
`Consolidator` runs a bounded cosine-clustering pass that supersedes or flags
contradicting memories.

## Dependencies & features

- `ort` (ONNX Runtime, `download-binaries` default — hermetic builds),
  `ndarray`, `tokenizers` (`onig`), `hnsw_rs`, `rusqlite` (`bundled`, `blob`),
  `ulid`, `blake3`, `bitvec`, `parking_lot`, `regex`, `rand`/`rand_chacha`,
  `ureq`, `tracing`, `thiserror`.
- Workspace crates: `origin-cas` (bodies), `origin-store` (SQLite). The `ort`
  pin (rc.12 / ONNX Runtime 1.24.2) is why the workspace MSRV moved to 1.96.
- Dev-deps: `proptest`, `tempfile`. No optional cargo features.

## Used by

`crates/*/Cargo.toml` matches for `origin-mem`:

- `crates/origin-cli/Cargo.toml`
- `crates/origin-daemon/Cargo.toml`
- `crates/origin-mem/Cargo.toml`
- `crates/origin-migrate/Cargo.toml`
- `crates/origin-skills/Cargo.toml`

## Testing

Integration tests under `crates/origin-mem/tests/`: `embedder.rs`, `index.rs`,
`quantizer.rs`, `proposer.rs`, `injector.rs`, `consolidator.rs`,
`storage_round_trip.rs`, `refcount_decrement.rs`, `time_constants.rs`.
`proptest` fuzzes the quantizer codec; `tempfile` backs the SQLite/CAS
round-trips.

## See also

- [Memory & code graph subsystem](../subsystems/memory-and-codegraph.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
