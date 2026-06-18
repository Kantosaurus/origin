# origin-stream

> Single-producer multi-consumer byte ring with per-subscriber read cursors

## Purpose

`origin-stream` is the in-process fan-out channel for streamed token events. One
producer (the agent loop / provider relay) appends `TokenEvent`s to a
fixed-capacity ring; any number of subscribers (renderer, IPC event relays) each
hold their own read cursor and consume independently. It is the SPMC primitive
behind live token streaming — Mechanism N2.1.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Ring` | struct | Cloneable handle to the shared ring; `with_capacity`, `publish`, `close`, `subscribe`. |
| `Subscriber` | struct | One independent tail; `next` (async), `try_next` (non-blocking). |
| `RingError` | enum | `Closed` / `TooLarge` / `Encode` / `Decode`. |
| `TokenEvent` | struct | The record type appended to the ring (re-exported from `event`). |
| `TokenKind` | enum | Variant tag for a `TokenEvent`. |
| `parse` | fn | Panic-free length-prefixed decoder used by fuzz targets. |

## Key types

```rust
/// Cloneable handle to the underlying ring.
#[derive(Clone)]
pub struct Ring { inner: Arc<Inner> }

impl Ring {
    pub fn with_capacity(capacity: usize) -> Self;
    pub fn publish(&self, ev: &TokenEvent) -> Result<(), RingError>;
    pub fn close(&self);
    pub fn subscribe(&self) -> Subscriber; // starts at the current write cursor
}

/// One tail. Each subscriber tracks its own read position.
pub struct Subscriber { ring: Ring, read_cursor: usize }

impl Subscriber {
    pub async fn next(&mut self) -> Result<Option<TokenEvent>, RingError>;
    pub fn try_next(&mut self) -> Result<Option<TokenEvent>, RingError>;
}
```

## How it works

Internally the ring is **one append-only `Mutex<Vec<u8>>` buffer plus an atomic
write cursor**. Each record is an `rkyv`-archived `TokenEvent` prefixed with a
`u32` big-endian length. `publish` archives the event, checks it fits the
remaining capacity (Phase 2 has no wrap-around — the ring is sized for one turn),
appends it under the lock, advances the write cursor with a `Release` store, and
calls `Notify::notify_waiters()`. After warmup the buffer never reallocates.

Each `Subscriber` holds its own `read_cursor`. `next` decodes the record at the
cursor when the read cursor is behind the write cursor, otherwise it parks on the
`Notify`. The wake path is carefully race-free: the `Notified` future is
constructed and re-checked **before** awaiting (Notify buffers only a single
permit), and on close the write cursor is re-loaded so a record published just
before `close()` is still delivered before `Ok(None)`.

```text
producer ──publish(ev)──► [len|rkyv ev][len|rkyv ev]... (Vec<u8>)
                                   ▲ write_cursor (atomic)
   sub A read_cursor ──┐
   sub B read_cursor ──┴──► each decodes independently; Notify wakes both
```

`try_next` lets a consumer drain a burst of ready records (and coalesce them)
without a task yield per record; unlike `next` it does not distinguish
"caught up" from "closed". `parse` is the same decode path hardened to never
panic on arbitrary bytes, used by fuzz targets.

## Dependencies & features

- `rkyv` (`validation` + `bytecheck`) — record encode/decode.
- `tokio` (`sync`) — `Notify` wakeups.
- `parking_lot` — the buffer mutex.
- `bytes`, `thiserror`.
- **`unsafe_code = "allow"`** for the raw cursor atomics;
  `undocumented_unsafe_blocks` is denied.

No cargo features are defined.

## Used by

Per `Grep "origin-stream" crates/*/Cargo.toml`: `origin-cli`, `origin-daemon`,
`origin-tui`, the provider adapters (`origin-provider`, `-anthropic`, `-bedrock`,
`-gemini`, `-ollama`, `-openai-compat`).

## Testing

`crates/origin-stream/tests/`: `multi_tail.rs` (independent subscriber cursors)
and `soak.rs` (sustained publish/consume). The decode path is additionally
fuzzed via `parse`.

## See also

- [../architecture/runtime-and-concurrency.md](../architecture/runtime-and-concurrency.md) — streaming and wakeup discipline.
- [../subsystems/agent-and-sessions.md](../subsystems/agent-and-sessions.md) — token streaming to the CLI/TUI.
- [../subsystems/providers.md](../subsystems/providers.md) — provider relays that feed the ring.
- Back to [../crates/README.md](../crates/README.md).

_Last reviewed against workspace version 0.9.8._
