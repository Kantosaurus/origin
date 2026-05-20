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

`origin-ipc::Connection` is the canonical entry point. It handles transport
selection (socket / pipe / QUIC), version handshake, and request-ID
multiplexing.

```rust
use origin_ipc::{Connection, Request, Event};
use origin_core::{Message, Role, Block};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Auto-detects ORIGIN_SOCK or the default platform path.
    let conn = Connection::connect_default().await?;

    // Open a fresh session.
    let session = conn
        .send(Request::OpenSession {
            model: Some("anthropic:claude-opus-4-7".into()),
            ..Default::default()
        })
        .await?
        .into_session()?;

    // Subscribe to the event stream before sending the prompt so we don't
    // miss the streamed assistant blocks.
    let mut events = conn.subscribe(session.id).await?;

    conn.send(Request::Prompt {
        session: session.id,
        message: Message {
            role: Role::User,
            blocks: vec![Block::text("List the workspace crates.")],
        },
    })
    .await?;

    while let Some(event) = events.next().await? {
        match event {
            Event::AssistantBlock(block) => print!("{}", block.as_text()),
            Event::ToolUse(tu)           => eprintln!("→ tool: {}", tu.name),
            Event::TurnComplete { .. }   => break,
            Event::PermissionAsk(ask)    => {
                // Side-panel-style async prompt; auto-allow in this demo.
                conn.send(Request::ApprovePermission(ask.id)).await?;
            }
            _ => {}
        }
    }

    conn.send(Request::CloseSession(session.id)).await?;
    Ok(())
}
```

The same `Connection` type powers the TUI in `origin-cli`, the headless
`origin run` subcommand, and the future desktop frontend. Tool-result blobs
arrive as `Event::ToolResult { handle, .. }`; call `conn.fetch(handle)` to
mmap the pack file and get back a `&[u8]` slice without touching userspace
copies on the daemon side.

For debugging IPC traffic see [Troubleshooting](troubleshooting.md) — the
trace parquet ring records every frame and replays into `origin-replay`
bundles deterministically.
