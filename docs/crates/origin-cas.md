# origin-cas

> Content-addressed store with FastCDC chunking, mmap pack files, and three-tier storage

## Purpose

`origin-cas` is the blob store behind every large tool result and message-log
shard. Content is addressed by its BLAKE3 hash, so identical bytes are stored
once and referenced by a 32-byte handle that flows through the IR (see
`origin-core::Block::ToolResult`). It backs the offload path that keeps
transcripts small: a big payload lands in CAS and only its handle is persisted.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Hash` | struct | 32-byte BLAKE3 content address; `Hash::of(bytes)`, `from_bytes`, `as_bytes`. |
| `Store` | struct | The three-tier store (Hot → Warm → Cold). |
| `StoreConfig` | struct | Tunables: root dir, hot capacity, warm pack target, cold zstd level. |
| `StoreError` | enum | `Io` / `Pack` / `Zstd` / `BadHotCapacity`. |
| `chunks` / `ChunkIter` / `ChunkRef` | fn/struct | FastCDC content-defined chunking (~16 KiB average). |
| `PackBuilder` / `PackReader` / `PackSlice` / `IndexEntry` | struct | Append-only, mmap-read pack-file format. |
| `PackError` | enum | Pack format errors (bad magic, truncation). |
| `RefTable` / `RefError` | struct/enum | Refcount table over SQLite (`incr`/`decr`/`get`/`dead_hashes`) for GC. |
| `DictVersion` / `DictError` | struct/enum | zstd dictionary training for the cold tier. |

Key `Store` methods: `open`, `put`, `get`, `flush_warm_pending`, `flush_all`,
`demote_to_cold`, `train_dict_from_sample`, `active_dict_version`.

## Key types

```rust
#[derive(Debug, Clone)]
pub struct StoreConfig {
    pub root: PathBuf,
    pub hot_capacity: usize,           // max entries in the Hot LRU
    pub warm_pack_target_bytes: u64,   // soft cap before a Warm pack is sealed
    pub cold_zstd_level: i32,          // zstd level for Cold (typical: 3)
}

/// Three-tier content-addressed store: Hot (LRU) → Warm (mmap) → Cold (zstd).
pub struct Store { /* inner: Mutex<Inner>, flush: Mutex<()> */ }
```

```rust
pub struct Hash([u8; 32]);
impl Hash {
    pub fn of(bytes: &[u8]) -> Self;          // BLAKE3
    pub const fn from_bytes(b: [u8; 32]) -> Self;
    pub const fn as_bytes(&self) -> &[u8; 32];
}
```

## How it works

Three tiers share one `Hash` namespace:

- **Hot** — an in-memory `LruCache<Hash, Vec<u8>>` bounded by `hot_capacity`.
- **Warm** — append-only **mmap'd** pack files on disk. LRU evictions accumulate
  in a pending batch; when the batch crosses `warm_pack_target_bytes` it is
  sealed into one `wNNNNNNNN.pack`.
- **Cold** — same pack format, but each payload is zstd-compressed (optionally
  with a trained dictionary) before append (`cNNNNNNNN.pack`).

`get(h)` walks **Hot → Warm-pending → Warm → Cold**; the first hit wins. `put`
dedupes against all tiers (and the pending batch) by hash before inserting.

```text
put ─► Hot LRU ──evict──► warm_pending ──seal──► Warm pack (mmap)
get ◄─ Hot ◄─ warm_pending ◄─ Warm ◄─ Cold (zstd-decompress)
                                  demote_to_cold ─► Cold pack (zstd)
```

Pack flushes are serialized by a dedicated `flush` mutex (always acquired before
`inner`) so two concurrent flushes can never pick the same pack filename, and on
failure the taken batch is restored to `warm_pending` so a recoverable I/O error
never silently drops already-`put` data. `flush_all` additionally copies
still-resident Hot entries into a warm pack **without evicting them**, which is
what keeps offloaded tool-result handles resolvable across a daemon restart (even
a SIGKILL that never runs a graceful shutdown).

FastCDC chunking (`chunks`) gives content-defined boundaries: a one-byte edit
shifts only the chunk containing it, so downstream chunks keep their addresses —
the basis of cross-turn dedup.

## Dependencies & features

- `blake3` (hashing), `fastcdc` (chunking), `memmap2` (pack reads), `lru`
  (Hot tier), `zstd` (Cold tier), `rusqlite` bundled (`RefTable`), `byteorder`,
  `parking_lot`, `thiserror`.
- **`unsafe_code = "allow"`** is set crate-wide (mmap + atomic cursors); every
  unsafe block carries a `// SAFETY:` comment and `undocumented_unsafe_blocks`
  is denied.
- Features: `recorder` (`dep:origin-replay`, installs a `CasTap`); `uring`
  (Linux-only `tokio-uring` write path, pulls in `origin-runtime` to attach a
  `Background` permit). A `uring_throughput` bench is gated on `uring`.

## Used by

Per `Grep "origin-cas" crates/*/Cargo.toml`: `origin-cli`, `origin-codegraph`,
`origin-daemon`, `origin-mcp`, `origin-mem`, `origin-plan`, `origin-sidecar`,
`origin-swarm`, `origin-tools`, `origin-tui`, the provider adapters
(`origin-provider`, `-anthropic`, `-bedrock`, `-gemini`, `-openai-compat`), and
`origin-cassette`.

## Testing

`crates/origin-cas/tests/`: `store.rs`, `hash.rs`, `packfile.rs`, `chunker.rs`,
`dict.rs`, `gc.rs`, `durability.rs`, `finalize_durability.rs`,
`uring_round_trip.rs`.

## See also

- [../architecture/data-and-storage.md](../architecture/data-and-storage.md) — the storage tiers and offload path.
- [../subsystems/agent-and-sessions.md](../subsystems/agent-and-sessions.md) — tool-result handles and rehydration.
- [origin-core.md](origin-core.md) — `Block::ToolResult` handles.
- Back to [../crates/README.md](../crates/README.md).

_Last reviewed against workspace version 0.9.8._
