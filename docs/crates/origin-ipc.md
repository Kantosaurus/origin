# origin-ipc

> Framed local-socket and QUIC/TLS IPC transports for origin

## Purpose

`origin-ipc` is the wire layer between the CLI, the daemon, and remote clients.
It defines a single length-prefixed frame format and two transports that carry
those frames: a local-socket / named-pipe `Connection` for same-machine traffic
and a QUIC + mutual-TLS connection for remote clients. Both transports expose
the same `read_frame` / `write_frame` / `write_raw` surface so the daemon's
dispatch loop is transport-agnostic.

## Public API surface

Modules: `frame`, `transport`, `quic`, `tls`, `instance` (and a feature-gated
`recorder_hook`).

| Item | Kind | Summary |
| --- | --- | --- |
| `frame::FrameKind` | enum | `Request` / `Response` / `Event` / `ErrorFrame` (`#[repr(u8)]`). |
| `frame::Frame<'a>` | struct | Parsed view: `{ request_id, kind, body }`. |
| `frame::encode` | fn | Encode `(request_id, kind, body)` into a `Vec<u8>`. |
| `frame::validate` | fn | Validate a slice into a borrowed `Frame`. |
| `frame::MAX_FRAME_BYTES` | const | 64 MiB body cap enforced by all readers. |
| `transport::Connection` | struct | Local-socket / named-pipe framed connection. |
| `transport::Listener` / `Connector` | struct | Bind / connect for the local transport. |
| `transport::SharedConnection` | type | `Arc<Mutex<Connection>>` for multi-writer use. |
| `transport::read_frame_from` / `read_frame_buffered` | fn | Standalone, cancellation-safe frame readers. |
| `quic::QuicListener` / `QuicConnector` / `QuicConnection` | struct | QUIC + mTLS transport mirroring `Connection`. |
| `tls::CertBundle`, `generate_self_signed`, `sha256_fingerprint(_hex)`, `parse_fingerprint_hex`, `fingerprints_eq` | struct/fn | Cert generation and SHA-256 fingerprint pinning. |
| `instance::InstanceId`, `resolve_ipc_path` | struct/fn | Per-working-directory instance addressing (socket / db / cas paths). |

## Key types

```rust
const MAGIC: u32 = 0x4F52_4F4E; // "ORON" big-endian
pub const HEADER_LEN: usize = 4 + 1 + 8 + 4; // magic + kind + id + body_len
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    pub request_id: u64,
    pub kind: FrameKind,
    pub body: &'a [u8],
}
```

```rust
pub struct Connection { /* inner IpcStream + rx_buf accumulator */ }

impl Connection {
    pub async fn read_frame(&mut self) -> io::Result<(FrameKind, Vec<u8>)>;
    pub async fn write_frame(&mut self, kind: FrameKind, body: &[u8]) -> io::Result<()>;
    pub async fn write_raw(&mut self, raw: &[u8]) -> io::Result<()>;
}
```

## How it works

Every frame is `magic(4) || kind(1) || request_id(8) || body_len(4 BE) || body`.
`encode` builds that buffer; `validate` checks magic, kind, and the length
invariant on a borrowed slice with no allocation.

The local transport reads through `read_frame_buffered`, which is
**cancellation-safe**: bytes are accumulated in a caller-owned `rx_buf` and a
frame is only consumed once fully buffered. A `read_frame` future dropped
mid-frame (e.g. a zero-timeout peek) leaves partial bytes intact for the next
call, so the stream never desynchronises. All readers reject a header
advertising a body larger than `MAX_FRAME_BYTES` *before* allocating, defending
against a hostile peer inducing a multi-GiB allocation.

The QUIC transport carries the identical frames over one bidirectional QUIC
stream and adds mutual TLS: peers exchange and **pin SHA-256 certificate
fingerprints** at pairing time (the `tls` module computes them; custom
`ServerCertVerifier` / `ClientCertVerifier` implementations enforce the pin).
`bind_bearer_gated` / `connect_with_bearer` add a bearer-token handshake on top.

```text
CLI ──local socket / named pipe──► Connection ──┐
                                                  ├─► daemon dispatch loop
remote client ──QUIC + mTLS──► QuicConnection ───┘   (transport-agnostic)
```

The local `Listener::bind` also reclaims a **stale Unix socket** left by a dead
daemon: on `AddrInUse` it confirms nothing live is serving the path before
unlinking, so it never clobbers a running daemon.

## Dependencies & features

- `interprocess` (tokio) — local socket / named pipe.
- `quinn`, `rustls`, `rustls-pemfile`, `rcgen`, `x509-parser` — QUIC + TLS.
- `sha2`, `hex` — fingerprints.
- `rkyv`, `bytes`, `parking_lot`, `tokio` — framing and async I/O.
- `origin-core` — IR types carried in bodies.
- **Feature `recorder`** (`dep:origin-replay`) — installs an `IpcTap` so frames
  feed the deterministic recorder (`recorder_hook::register_tap` / `tap`).

## Used by

Per `Grep "origin-ipc" crates/*/Cargo.toml`: `origin-cli`, `origin-daemon`,
`origin-supervisor` (and `origin-ipc` itself).

## Testing

`crates/origin-ipc/tests/`: `frame.rs`, `frame_prop.rs`, `transport_smoke.rs`,
`stale_socket_reclaim.rs`, `handshake.rs`, `tls.rs`, `quic_smoke.rs`,
`quic_concurrent.rs`. `transport.rs` also has an in-file `#[cfg(test)]` module
covering the oversized-length rejection and cancellation-safe framing.

## See also

- [../architecture/overview.md](../architecture/overview.md) — process topology and the IPC boundary.
- [../security/security-model.md](../security/security-model.md) — mTLS fingerprint pinning and bearer gating.
- [../subsystems/agent-and-sessions.md](../subsystems/agent-and-sessions.md) — how requests/events ride the framing.
- Back to [../crates/README.md](../crates/README.md).

_Last reviewed against workspace version 0.9.8._
