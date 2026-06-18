# origin-replay

> Deterministic record-and-replay for origin sessions

## Purpose

`origin-replay` captures every source of non-determinism in a session — provider
HTTP traffic, IPC frames, CAS writes, the clock, and the RNG — into a single
compressed bundle, so a session can be replayed byte-for-byte offline. Each
boundary is a *tap* that the surrounding crate calls; in replay mode the same
taps serve recorded data instead of touching the network, disk-randomness, or the
wall clock. (See spec §10C, mechanisms N10.7–N10.8.)

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `recorder::Recorder` | trait | `record(Frame)` + `close()` — the sink every tap writes to. |
| `recorder::Frame` | enum | One recorded event (provider/IPC/CAS/clock/RNG). |
| `recorder::FileRecorder` / `NullRecorder` | struct | Append-to-file sink / inert sink. |
| `bundle::BundleWriter` / `Bundle` / `Manifest` | struct | Write / read a zstd+tar replay bundle (`ORIGREP1`). |
| `bundle::BundleError` | enum | Bundle I/O / magic / manifest errors. |
| `clock::Clock` / `SystemClock` / `VirtualClock` | trait/struct | Real vs. recorded `now()`. |
| `rng::Rng` / `SeededRng` | trait/struct | Seeded RNG hooked through the recorder. |
| `provider_tap::ProviderTap` / `ReplayProvider` | struct | Record/serve provider requests + streamed chunks. |
| `ipc_tap::IpcTap` | struct | Tap for inbound/outbound IPC frames. |
| `cas_tap::CasTap` | struct | Tap for CAS write fingerprints. |

## Key types

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Frame {
    ProviderRequest { id: u64, body_blake3: [u8; 32] },
    ProviderResponseChunk { id: u64, seq: u32, body: Vec<u8> },
    ProviderResponseEnd { id: u64 },
    IpcInbound { conn: u32, body: Vec<u8> },
    IpcOutbound { conn: u32, body: Vec<u8> },
    CasWrite { handle_hex: String, size: u64 },
    Clock { seq: u64, unix_ms: u64 },
    Rng { seq: u64, bytes: Vec<u8> },
}

pub trait Recorder: Send + Sync {
    fn record(&self, frame: Frame);
    fn close(&self);
}
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub session_id: String,
    pub recorded_at_unix_ms: u64,
    pub origin_version: String,
}
```

## How it works

Every non-deterministic boundary writes **one `Frame` per event** to a
`Recorder`. The `provider_tap` records each request's BLAKE3 fingerprint plus the
ordered streamed chunks; the `ipc_tap` records assembled wire frames in both
directions; the `cas_tap` records `(handle_hex, size)` after a blob is durably
stored; the `VirtualClock` and `SeededRng` record each timestamp and random draw
with a monotonically increasing `seq`.

The recorder crates expose **install hooks** rather than calling into
`origin-replay` directly (which would create a dependency cycle): `origin-ipc`
and `origin-cas` each have a feature-gated `recorder_hook::register_tap` that
stores an `Arc<IpcTap>` / `Arc<CasTap>` in a process-global `RwLock`. This keeps
the recorder optional and the leaf crates dependency-light.

On replay, a `Bundle` (a `zstd`-compressed `tar` archive with magic `ORIGREP1`
and a `manifest.json`) is opened and a `ReplayProvider` / `VirtualClock` /
`SeededRng` serve the recorded frames in order — no network, no real clock, no
fresh entropy — so the session is reproduced byte-identically.

```text
record:  taps ──Frame──► Recorder (FileRecorder) ──► BundleWriter (zstd+tar)
replay:  Bundle ──Frame──► ReplayProvider / VirtualClock / SeededRng ──► agent loop
```

## Dependencies & features

- `origin-core` — IR types referenced by taps.
- `tar`, `zstd` — bundle container.
- `serde` / `serde_json` — frame + manifest encoding.
- `blake3` — request fingerprints.
- `tokio` (`sync`, `io-util`), `parking_lot`, `thiserror`.

No cargo features in this crate; consumers enable their own `recorder` feature
(e.g. `origin-ipc`, `origin-cas`) which pulls this in.

## Used by

Per `Grep "origin-replay" crates/*/Cargo.toml`: `origin-bench`, `origin-cas`
(optional, `recorder` feature), `origin-ipc` (optional, `recorder` feature),
`origin-provider`.

## Testing

`crates/origin-replay/tests/`: `round_trip.rs` (record → bundle → replay) and
`determinism.rs` (byte-identical reproduction).

## See also

- [../architecture/overview.md](../architecture/overview.md) — where taps sit relative to providers/IPC/CAS.
- [origin-cassette.md](origin-cassette.md) — the narrower HTTP-only fixture format.
- [origin-ipc.md](origin-ipc.md) / [origin-cas.md](origin-cas.md) — the `recorder` feature hooks.
- Back to [../crates/README.md](../crates/README.md).

_Last reviewed against workspace version 0.9.8._
