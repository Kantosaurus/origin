# SDK

The `origin` daemon exposes a stable wire protocol via `origin-ipc`. Any
process that can speak it — a TUI, a CI script, a future desktop frontend —
is an `origin` client. The CLI is just the first client.

## Core IR types

`origin-core` defines the canonical Intermediate Representation that flows
through the entire system. Three types are load-bearing:

- **`Message { role: Role, blocks: Vec<Block> }`** — the unit of conversation.
  `role` is `User | Assistant | System | Tool`. `blocks` is a heterogeneous
  list because modern providers return interleaved text, tool calls, and
  thinking in a single turn.
- **`Block`** — `Text { content, cache_marker }`, `ToolUse { id, name, input }`,
  `ToolResult { tool_use_id, handle: CasHandle, preview }`, `Thinking`,
  `Image`, `Audio`. Each provider's projection drops blocks it can't carry
  (lossily where allowed, with a compile-time check when the caller marked
  them required).
- **`ToolCall { name, normalized_input, id }`** — what the model emits.
  `normalized_input` is the canonicalized JSON used for memoization (same
  call within a session returns the cached `ToolResult` handle without
  re-executing).

All three are `#[derive(rkyv::Archive, ...)]`, so the same byte buffer flows
through IPC, SQLite blob columns, and in-memory ring buffers — no
serialize/deserialize hops on the hot path.

## Wire protocol

`origin-ipc` runs a single duplex stream per client, request-ID multiplexed.
On Linux/macOS that's a Unix socket; on Windows a named pipe. Remote clients
use QUIC + mutual TLS. The wire format is `rkyv`-archived: validation costs
~200ns vs. ~20µs to JSON-decode the same payload. Blob payloads never travel
over the stream — they cross via shared file mappings carrying a
`SharedHandle { pack_file, offset, len, hash }`, which the client mmaps
directly.

Surface, all versioned with a `v: u16`:

```
Session:    open, prompt, interrupt, close
Inspect:    list_sessions, list_workers, get_message, get_plan
Stream:     subscribe(session_id) → Event stream
Tool:       approve_permission, deny_permission, override_tier
Memory:     search, save, forget, list_proposals
Code graph: query, path, explain, summarize, rebuild
Config:     get, set, reload_skills, reload_hooks
Admin:      gc_cas, vacuum_db, switch_account, attach_swarm
```

Streams are credit-backpressured: a slow client just stops getting events
without back-pressuring the main loop.

## Minimal Rust client

Today `origin-ipc` exposes a **frame-level** API. The building blocks are:

- `origin_ipc::transport::{Connector, Connection}` — connect to the daemon's
  local socket / named pipe and read/write framed messages.
- `origin_ipc::quic::{QuicConnector, QuicConnection}` — the same framing over
  QUIC + mutual TLS for remote clients.
- `origin_ipc::frame::{encode, validate, FrameKind}` — the length-prefixed
  framing. `FrameKind` is `Request | Response | Event | ErrorFrame`; bodies are
  `rkyv`-archived protocol messages.

A client connects, writes a `Request` frame, and reads `Response`/`Event`
frames until the turn completes. The daemon discovers its socket from
`ORIGIN_SOCK` (falling back to a per-platform default), so a client reads the
same variable:

```rust
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::Connector;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Same path the daemon binds: ORIGIN_SOCK, else the platform default.
    let path = std::env::var("ORIGIN_SOCK")?;
    let mut conn = Connector::connect(&path).await?;

    // `body` is an rkyv-archived request message (an "open session" / "prompt"
    // verb from the surface above). `encode(request_id, kind, body)` prepends
    // the framing header; request_id lets you correlate the Response.
    let body: &[u8] = /* rkyv-archived request bytes */ b"";
    conn.write_raw(&encode(1, FrameKind::Request, body)).await?;

    // Read frames until the turn ends. Events stream incrementally; the final
    // Response (or an ErrorFrame) closes the request.
    loop {
        let (kind, payload) = conn.read_frame().await?;
        match kind {
            FrameKind::Event => { /* decode + render the streamed block */ }
            FrameKind::Response => break, // turn complete
            FrameKind::ErrorFrame => {
                anyhow::bail!("daemon error: {}", String::from_utf8_lossy(&payload));
            }
            FrameKind::Request => {} // clients don't receive requests
        }
    }
    Ok(())
}
```

The CLI's headless path (`origin run`) and the TUI both drive the daemon
through exactly this transport — see `crates/origin-cli/tests/headless_stream.rs`
for a worked example that stands up a fake daemon and exchanges frames.

> **Planned:** an ergonomic typed client — `Connection::connect_default()`
> plus `Request` / `Event` enums and a `subscribe()` stream that hide the frame
> encoding and request-ID multiplexing — is on the roadmap but **not yet
> implemented**. Until it lands, build against the frame-level API above.

For debugging IPC traffic see [Troubleshooting](troubleshooting.md) — the
trace parquet ring records every frame and replays into `origin-replay`
bundles deterministically.
