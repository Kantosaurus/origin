# origin-trace

> Tracing layer that writes spans to a per-day parquet ring with queryable predicates

## Purpose

`origin-trace` turns `tracing` span closes into structured rows persisted as
Snappy-compressed Apache Parquet, rotating files at 64 MiB so a long-lived
daemon keeps a bounded, postmortem-queryable trace history. A background drain
thread owns the writer so the foreground agent loop only pays for a non-blocking
channel send. A separate query module reads the parquet files back with
column-level predicates on `kind` and `error_kind`.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `init(dir)` | fn → `Result<LayerGuard, RingError>` | Install the global subscriber writing to `dir`; returns a flush-on-drop guard. |
| `Layer` | struct | `tracing_subscriber::Layer` capturing `on_close` into `SpanRow`s. |
| `LayerGuard` | struct (`#[must_use]`) | Drop guard that flips the kill switch and joins the drain thread. |
| `Ring` | struct | Per-day parquet writer with 64 MiB rotation. |
| `Ring::open(dir, cap_bytes)` | fn → `Result<Ring, RingError>` | Open/create the ring directory. |
| `Ring::append(row)` / `flush()` | fn | Buffer one row / drain buffered rows to parquet. |
| `RingError` | enum | `Io` / `Parquet` / `Arrow`. |
| `QueryArgs` | struct | `{ dir, kind, error_kind, limit }` pushdown filter. |
| `run(args)` | fn → `Result<Vec<QueryRow>, QueryError>` | Stream every `.parquet` file under `dir`, filter, return up to `limit`. |
| `QueryRow` / `QueryError` | struct / enum | One decoded row / read errors. |
| `span_schema()` | fn → `Arc<Schema>` | Canonical Arrow schema for a span row. |
| `SpanRow` | struct | The row appended by `Ring` and reconstructed on read. |

## Key types

```rust
#[derive(Debug, Clone)]
pub struct SpanRow {
    pub ts_ns: u64,
    pub span_id: u64,
    pub parent_id: u64,
    pub kind: &'static str,
    pub provider: &'static str,
    pub tool: &'static str,
    pub dur_us: u64,
    pub error_kind: &'static str,
    pub attrs_json: String,
}

#[derive(Debug, Clone)]
pub struct QueryArgs {
    pub dir: PathBuf,
    pub kind: Option<String>,
    pub error_kind: Option<String>,
    pub limit: usize,
}
```

The Arrow schema declares nine non-nullable columns (`ts_ns`, `span_id`,
`parent_id`, `kind`, `provider`, `tool`, `dur_us`, `error_kind`, `attrs_json`)
— the ring writer is the single source of truth for that layout.

## How it works

`Layer::on_new_span` stashes a start `Instant`, the interned `kind`/`provider`/
`tool`/`error_kind` strings, and a hand-rolled JSON attrs blob into the span's
extensions. `on_close` computes `dur_us`, stamps a wall-clock `ts_ns`, and does a
non-blocking `try_send` into a `sync_channel` (capacity 4096). A dropped row is
preferred over a blocked agent loop.

```
tracing span close
      │  try_send(SpanRow)        ┌─ origin-trace-drain thread ─┐
      ▼                           │  recv_timeout(25ms)         │
  SyncSender ───────────────────► │  Ring::append → batch(4096) │
                                  │  rotate at 64 MiB           │
                                  └──────────► trace-DATE-MS-SEQ.parquet
```

`Ring` buffers into Arrow builders, flushing a `RecordBatch` every 4096 rows or
on `flush()`/`Drop`, and rotates to a new `trace-<date>-<ms>-<seq>.parquet` file
once the estimated bytes would exceed `cap_bytes`. Static strings are interned
through a deduplicating pool capped at 4096 entries (returning
`<interned-pool-full>` past the cap) so a long-running daemon does not leak
memory on distinct tool/error names. `init` additionally installs a
human-readable `fmt` layer writing `<data>/logs/daemon.log`, with verbosity from
`ORIGIN_LOG`, then `RUST_LOG`, defaulting to `info`.

`query::run` lists `.parquet` files, sorts them lexicographically (file names
embed an ISO date + ms timestamp so this matches creation order), decodes each
`RecordBatch`, and emits matching rows. A missing directory is treated as "no
traces yet". The `limit` is checked before each push, so `limit == 0` yields
zero rows.

## Dependencies & features

- `arrow` / `parquet` — record batches + Snappy-compressed columnar files.
- `tracing` / `tracing-subscriber` — the layer + `fmt` text log.
- `chrono` — UTC date/timestamp components for file names.
- `serde` / `serde_json` — attribute helpers.
- `thiserror` — `RingError` / `QueryError`.
- Dev: `tempfile`; a `write` Criterion-style bench (`harness = false`).

No cargo features; the crate is always built with the full ring + query surface.

## Used by

`Grep "origin-trace" glob "crates/*/Cargo.toml"`:

- `crates/origin-cli/Cargo.toml`
- `crates/origin-daemon/Cargo.toml`
- `crates/origin-trace/Cargo.toml` (self)

## Testing

Integration tests live under `tests/`: `ring.rs` (append/rotate/flush
behaviour), `query.rs` (predicate pushdown + limit semantics), and `layer.rs`
(span → row capture). Inline unit tests in `layer.rs` cover the interning cap +
sentinel, the `now_ns` wall-clock invariant, and the `logs` sibling-directory
path resolution. A `benches/write.rs` benchmark exercises the append path.

## See also

- [Observability subsystem](../subsystems/observability.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
