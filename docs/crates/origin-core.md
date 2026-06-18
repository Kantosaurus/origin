# origin-core

> Core message types, IR, and provider capabilities for origin

## Purpose

`origin-core` holds the load-bearing data types that flow through the entire
system: the conversation IR (`Message`, `Block`, `Role`) and the shared
provider/capability descriptors. It sits at the very bottom of the crate graph —
it has no internal dependencies — so every transport, store, and provider crate
can speak the same wire vocabulary. Because the IR types are `rkyv`-archivable,
a single byte buffer can travel through IPC frames, SQLite blobs, and in-memory
ring buffers without being re-encoded on the hot path.

## Public API surface

The crate exposes two modules: `ir` (provider capability descriptors) and
`types` (the message IR).

| Item | Kind | Summary |
| --- | --- | --- |
| `types::Role` | enum | `User` / `Assistant` / `Tool` / `System`; `#[repr(u8)]`, rkyv-archivable. |
| `types::Block` | enum | A message content block: `Text`, `ToolUse`, `ToolResult`, `Thinking`. |
| `types::Message` | struct | `{ role: Role, blocks: Vec<Block> }` — the unit of conversation. |
| `types::MessageId` | struct | ULID-backed message identifier (`MessageId::new()`). |
| `types::TurnIndex` | struct | `u32` turn counter with a checked `next()`. |
| `types::CacheBoundary` | enum | `Frozen` / `Sticky` / `Sliding` prompt-cache markers. |
| `types::strip_orphan_tool_results` | fn | Repairs a transcript by dropping `ToolResult`s with no matching `ToolUse`. |
| `ir::ProviderCaps` | struct | Per-provider capability flags (cache kind, thinking, vision, …). |
| `ir::CacheKind` | enum | `None` / `Implicit` / `Explicit` prompt-cache support. |

## Key types

```rust
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[archive(check_bytes)]
pub enum Block {
    Text { text: String, cache_marker: Option<CacheBoundary> },
    ToolUse { id: String, name: String, input_json: Vec<u8>, cache_marker: Option<CacheBoundary> },
    ToolResult { tool_use_id: String, handle: Option<[u8; 32]>, inline: Option<Vec<u8>>, cache_marker: Option<CacheBoundary> },
    Thinking { tokens: String, signature: Option<String> },
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[archive(check_bytes)]
pub struct Message {
    pub role: Role,
    pub blocks: Vec<Block>,
}
```

```rust
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy)]
pub struct ProviderCaps {
    pub prompt_cache: CacheKind,
    pub thinking: bool,
    pub parallel_tools: bool,
    pub vision: bool,
    pub audio: bool,
}
```

## How it works

The IR is designed around **zero-copy archival**. Every IR type derives
`rkyv::Archive` with `#[archive(check_bytes)]`, which means a `Message` can be
serialized to a byte buffer once and then validated-in-place (`check_bytes`)
wherever it is read — over an IPC frame, out of a SQLite blob, or off the
SPMC ring in `origin-stream`. There is no intermediate JSON/serde hop on the
hot path.

`Block::ToolResult` deliberately carries either an inline `Vec<u8>` payload **or**
a 32-byte `handle` (a CAS content address). Large tool outputs are offloaded to
`origin-cas` and referenced by handle, keeping the archived transcript small.

`strip_orphan_tool_results` is a self-healing pass run right before a transcript
is loaded or sent to a provider. The Anthropic Messages API rejects a
`tool_result` whose `tool_use_id` has no matching `tool_use` in the preceding
message with a hard `400`; corruption from a reused session id, a compaction
hole, or a hand-edited store would otherwise be fatal. The function evaluates
"previous message" against the *repaired* output so dropping one orphaned turn
never knocks a valid pair out of alignment, and a well-formed transcript is
returned byte-identical.

```text
Message { role, blocks: [ Text | ToolUse | ToolResult | Thinking ] }
                                              │
                          inline Vec<u8>  ────┤
                          OR  handle [u8;32] ─┘──► origin-cas content address
```

## Dependencies & features

- `rkyv` (with `validation` + `bytecheck`) — the archival/zero-copy backbone.
- `ulid` (serde) — backs `MessageId`.
- `thiserror` — error definitions.
- Dev: `proptest` for property tests of the IR.

No cargo features are defined.

## Used by

Per `Grep "origin-core" crates/*/Cargo.toml`:
`origin-bench`, `origin-daemon`, `origin-ipc`, `origin-migrate`, `origin-provider`
and the provider adapters (`origin-provider-anthropic`, `-bedrock`, `-gemini`,
`-github`, `-ollama`, `-openai-compat`), `origin-replay`, `origin-sidecar`,
`origin-swarm`, and `origin-tools`.

## Testing

Integration tests live in `crates/origin-core/tests/`: `messages.rs`, `types.rs`,
and `ir.rs`. In addition, `types.rs` carries an in-file `#[cfg(test)]`
`tool_pairing_tests` module exercising `strip_orphan_tool_results`.

## See also

- [../architecture/overview.md](../architecture/overview.md) — where the IR sits in the system.
- [../architecture/data-and-storage.md](../architecture/data-and-storage.md) — how IR archives flow into CAS and SQLite.
- [../subsystems/providers.md](../subsystems/providers.md) — how `ProviderCaps` drives provider behaviour.
- Back to [../crates/README.md](../crates/README.md).

_Last reviewed against workspace version 0.9.8._
