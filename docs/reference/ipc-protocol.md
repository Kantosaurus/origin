# IPC Protocol

The `origin-ipc` crate defines the wire protocol between the `origin` CLI/TUI
clients and the `origin-daemon`. Two transports carry the **same** length-prefixed
frame format:

- **Local socket / named pipe** (`transport.rs`) — Unix domain socket on
  Linux/macOS, named pipe on Windows, via the `interprocess` crate.
- **QUIC + mutual TLS** (`quic.rs`, `tls.rs`) — for remote clients; same frames
  ride a single bidirectional QUIC stream.

See also: [`../crates/origin-ipc.md`](../crates/origin-ipc.md) ·
[`../crates/origin-daemon.md`](../crates/origin-daemon.md) ·
[`../security/security-model.md`](../security/security-model.md) ·
[environment-variables.md](environment-variables.md)

---

## Frame format

Source of truth: `crates/origin-ipc/src/frame.rs`.

```
 0        4    5             13            17                    17+len
 +--------+----+-------------+-------------+----------------------+
 | MAGIC  |kind|  request_id |  body_len   |        body          |
 |  u32   | u8 |    u64      |    u32       |      [u8; len]        |
 +--------+----+-------------+-------------+----------------------+
   big-endian       big-endian   big-endian
```

| Field | Offset | Size | Encoding | Notes |
|-------|--------|------|----------|-------|
| `MAGIC` | 0 | 4 | big-endian `u32` | `0x4F52_4F4E` — ASCII `"ORON"`. |
| `kind` | 4 | 1 | `u8` | `FrameKind` discriminant (see below). |
| `request_id` | 5 | 8 | big-endian `u64` | Correlates request/response; `0` for the convenience `write_frame`. |
| `body_len` | 13 | 4 | big-endian `u32` | Length of `body` in bytes. |
| `body` | 17 | `body_len` | bytes | Serialized payload (rkyv / JSON for the daemon protocol). |

Constants:

| Name | Value | Meaning |
|------|-------|---------|
| `MAGIC` | `0x4F52_4F4E` (`"ORON"`) | Frame sentinel; mismatch ⇒ `FrameError::BadMagic`. |
| `HEADER_LEN` | `17` (`4 + 1 + 8 + 4`) | Fixed header size. |
| `MAX_FRAME_BYTES` | `64 * 1024 * 1024` (64 MiB) | Hard cap on advertised body length. |

### FrameKind

`#[repr(u8)]` enum (`frame.rs`):

| Variant | Byte | Direction | Meaning |
|---------|------|-----------|---------|
| `Request` | `1` | client → daemon | A `ClientMessage` request. |
| `Response` | `2` | daemon → client | Terminal reply for a request. |
| `Event` | `3` | daemon → client | Streaming `StreamEvent` mid-turn. |
| `ErrorFrame` | `4` | daemon → client | Protocol/transport error. |

Any other byte ⇒ `FrameError::UnknownKind(x)` (`InvalidData` over the wire).

### Encoding & validation

`encode(request_id, kind, body) -> Vec<u8>` writes
`MAGIC | kind | request_id | body_len | body` using big-endian puts. It panics
only if `body.len() > u32::MAX` (not a realistic case).

`validate(bytes) -> Result<Frame, FrameError>` checks, in order:

1. `bytes.len() >= HEADER_LEN` else `Truncated`.
2. `MAGIC` matches else `BadMagic`.
3. `kind` byte ∈ {1,2,3,4} else `UnknownKind`.
4. `bytes.len() == HEADER_LEN + body_len` else `LengthMismatch`.

| `FrameError` | Cause |
|--------------|-------|
| `Truncated` | Slice shorter than the 17-byte header. |
| `BadMagic` | Leading 4 bytes ≠ `"ORON"`. |
| `UnknownKind(u8)` | `kind` byte not 1–4. |
| `LengthMismatch` | `body_len` field disagrees with actual remaining bytes. |

---

## Reading frames (DoS-hardening + cancellation safety)

`transport.rs` exposes two async readers:

- `read_frame_from<R>(reader)` — reads the fixed header, then the body. **Before
  allocating**, it rejects any `body_len > MAX_FRAME_BYTES` with
  `io::ErrorKind::InvalidData` ("frame too large"). A hostile peer cannot induce
  a multi-GiB allocation with a crafted length header.
- `read_frame_buffered<R>(reader, rx_buf)` — the cancellation-safe variant used
  by the live `Connection`. Bytes accumulate in a caller-owned `rx_buf`; a frame
  is consumed only once fully buffered. If the read future is dropped mid-frame
  (e.g. a zero-timeout peek), the partial bytes survive in `rx_buf` and the next
  call resumes — the stream never desynchronises.

`Connection` (local socket / named pipe) methods:

| Method | Purpose |
|--------|---------|
| `read_frame_body()` | Read next frame, return body bytes (kind discarded). |
| `read_frame()` | Read next frame, return `(FrameKind, body)`. |
| `write_frame(kind, body)` | Encode with `request_id = 0` and flush. |
| `write_raw(raw)` | Write a pre-encoded frame (for non-zero ids). |

`Listener::bind(path)` reclaims a **stale** Unix socket (file exists but no live
listener answers `connect`) but never clobbers a running daemon. The socket path
defaults via `default_path()` and is overridable with `ORIGIN_SOCK`.

---

## QUIC + mutual TLS (remote)

`quic.rs` mirrors the `read_frame` / `write_frame` / `write_raw` surface so the
daemon dispatch loop is transport-agnostic. Each connection uses **one
bidirectional QUIC stream**; request/response pairs and event streams share that
ordered byte channel, exactly like the local-socket transport. The same
`MAX_FRAME_BYTES` cap and `FrameKind` parsing apply.

**Trust model (zero-trust, fail-closed):**

- Mutual TLS — both peers present certificates.
- Authentication anchor is the **SHA-256 certificate fingerprint** (`tls.rs`,
  `CertFingerprint`), pinned at pairing time (`PairStart` / `PairRedeem`).
- `QuicListener::bind` accepts only clients whose fingerprint is in
  `allowed_clients`; an **empty** allow-list trusts no peer (denies access
  rather than silently opening it).
- Crypto provider is rustls + `ring` (TLS 1.3, X25519, Ed25519/ECDSA). The
  fingerprint anchor remains sound against a quantum adversary; migrating the key
  exchange to a hybrid PQ group is a drop-in provider swap (tracked in
  `SECURITY.md`).

Remote client certs/keys are supplied via `ORIGIN_REMOTE_CLIENT_CERT_FILE` and
`ORIGIN_REMOTE_CLIENT_KEY_FILE` (see [environment-variables.md](environment-variables.md)).

---

## Daemon protocol payloads

The `body` of `Request`/`Response`/`Event` frames carries the daemon protocol
types from `crates/origin-daemon/src/protocol.rs`.

### ClientMessage (Request frames)

Internally tagged on `kind` (snake_case). Selected variants:

| Variant | Purpose |
|---------|---------|
| `Prompt(PromptRequest)` | Run a user prompt through the agent loop. |
| `PermissionDecision { id, allow, always }` | Answer a `PermissionAsk` (the `always` flag remembers the decision). |
| `ChoiceDecision { id, selected, custom }` | Answer an `ask_user` `ChoiceAsk`. |
| `Interrupt` / `ClearAll` | Cancel an in-flight goal / reset context. |
| `SwitchAccount` | Hot-swap provider/account credential. |
| `MemoryDecision` | Accept/reject a proposed memory. |
| `PairStart` / `PairRedeem` | QUIC pairing handshake (6-digit code → device binding). |
| `ListSessions` / `RemoveSession` / `RewindSession` / `ResumeSession` / `ResumeForeign` | Session admin. |
| `GetUsage` | Per-provider/model token usage snapshot. |
| `KeyringAdd` / `KeyringList` / `KeyringRemove` | Keyvault admin. |
| `ResumeRequest { token }` | Supervisor → daemon resume on restart. |
| `ActivateSkill` / `DeactivateSkill` / `ActivateWorkflow` | Skill / workflow stack control. |
| `SubscribePlan` | Subscribe to the plan-op broadcast. |
| `ExportSession` | Export a transcript as `md` or `json`. |
| `SelfDevStart` / `SelfDevStatus` / `SelfDevApprove` / `SelfDevReset` | Self-development control plane (gated by `ORIGIN_SELFDEV=1`). |

### StreamEvent (Event frames)

Mid-turn streaming events. Selected variants:

| Variant | Purpose |
|---------|---------|
| `TextDelta { text }` | Assistant prose chunk. |
| `ToolUseDelta { partial_json }` | Streaming tool-call args. |
| `ThinkingDelta { thinking }` | Reasoning chunk. |
| `Usage { input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens }` | Token accounting (note the cache fields). |
| `ToolActivity { tool, summary, diff_lines }` | Tool start line. |
| `ToolChunk { tool, content }` | Live incremental tool output (today: `Bash`). |
| `ToolResult { tool, ok, preview, elided_bytes }` | Post-dispatch result preview. |
| `SwarmWorker { id, goal, status, detail, tool }` | Per-sub-agent lifecycle (`spawned`/`running`/`completed`/`failed`). |
| `SwarmAgentOutput { id, part, body, ok }` | One line of a sub-agent's live transcript. |
| `PermissionAsk { id, tool, args_preview }` | Opt-in interactive permission gate. |
| `ChoiceAsk { id, … }` | `ask_user` structured choice. |
| `TurnEnd` | End of the assistant turn. |

`ServerMessage` (Response frames) carries terminal replies such as `PromptReply
{ assistant_text, turns }` and the various admin acks/errors referenced above.

> **Compatibility:** new `ClientMessage` variants are appended so the wire layout
> of existing variants is preserved; additive fields use `#[serde(default)]`
> (e.g. `PermissionDecision.always`). Legacy clients sending a raw
> `PromptRequest` JSON body are handled by a daemon fallback.

---

_Last reviewed against workspace version 0.9.8._
