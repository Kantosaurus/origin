# origin-provider

> Provider trait with canonical request/response/usage/error types for origin

## Purpose

`origin-provider` defines the single abstraction every backend implements: the
[`Provider`] trait plus the canonical, wire-agnostic request/response/usage/error
types it exchanges. Each per-provider crate translates these canonical types to
and from its own wire format, so the daemon can drive any backend through one
interface. The crate also ships the static provider catalog, shared SSE/NDJSON
stream adapters, OpenAI tool-call decoding, and the CAS handle-inflation helper
used by all providers.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Provider` | trait | `name`, `base_url`, `chat`, and a default `chat_stream` every backend implements. |
| `ChatRequest` | struct | Canonical request: `system`, `messages`, `model`, `tools`, `effort`, `attachments`, `thinking_tokens`. |
| `ChatResponse` | struct | Canonical response: `assistant: Message` + `usage`. |
| `Usage` | struct | Token counts: input/output/cache-read/cache-creation. |
| `ToolSchema` | struct | Tool advertised to the model: `name`, `description`, `input_schema_json`. |
| `ReasoningEffort` | enum | Effort tier (`Fast`..`Ultracode`) with `as_anthropic_effort`/`as_openai_effort`/wire mapping. |
| `ProviderError` | enum | `Transport`, `Api`, `Auth`, `RateLimit { retry_after_secs, message }`. |
| `inflate_tool_result_handles` | fn | Resolve CAS-handle-backed `ToolResult` blocks into inline bytes. |
| `CAS_MISS_PLACEHOLDER` | const | Inlined notice when a tool-result CAS payload is lost. |
| `catalog::{Catalog, ProviderEntry, WireFormat, AuthScheme, Capabilities}` | mod | Static provider catalog + merge of custom entries. |
| `builtin_catalog` | fn | Returns the built-in `Vec<ProviderEntry>`. |
| `custom::load` | fn | Load user-defined `ProviderEntry` rows from a TOML file. |
| `sse::from_reqwest` / `ndjson::from_reqwest` | fn | Shared stream adapters over a `reqwest::Response`. |
| `openai_tools::{WireToolCall, tool_call_to_block}` | mod | Decode OpenAI `tool_calls` into canonical `Block::ToolUse`. |

## Key types

```rust
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn base_url(&self) -> Option<&str> { None }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError>;
    async fn chat_stream(&self, req: ChatRequest, ring: &origin_stream::Ring)
        -> Result<(), ProviderError> { /* default: chat() → one TextDelta + TurnEnd */ }
}

#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub model: String,
    pub tools: Vec<ToolSchema>,
    pub effort: Option<ReasoningEffort>,
    pub attachments: Vec<origin_multimodal::ContentBlock>,
    pub thinking_tokens: Option<u32>,
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("transport: {0}")] Transport(String),
    #[error("api: {0}")] Api(String),
    #[error("auth")] Auth,
    #[error("rate limit; retry after {retry_after_secs}s")]
    RateLimit { retry_after_secs: u32, message: String },
}
```

## How it works

The trait is the contract: `chat` runs a single non-streaming turn, and
`chat_stream` publishes `TokenEvent`s into an `origin_stream::Ring`. Providers
that lack native streaming inherit the default `chat_stream`, which calls `chat`
and emits one `TextDelta` followed by `TurnEnd`.

`ReasoningEffort` is wire-agnostic on the canonical side and maps per backend:
Anthropic accepts only `low|medium|high|xhigh|max` (so `Fast` → `low`,
`Ultracode` → `max`), while OpenAI tops out at `high`. `None` leaves the wire
byte-identical to the pre-effort behaviour.

`inflate_tool_result_handles` is the shared bridge between the daemon's
content-addressed storage and provider encoders that only understand inline
bytes. The daemon stores every tool result as a CAS handle; before encoding, a
provider resolves the handle to bytes. A *missing* CAS is a hard
`ProviderError::Api`, but a CAS *miss* (payload lost across a daemon restart)
degrades gracefully to [`CAS_MISS_PLACEHOLDER`], keeping the
`tool_use`/`tool_result` pairing intact so the turn continues.

The `catalog` module enumerates supported providers as `ProviderEntry` rows,
each tagged with a `WireFormat` (`OpenAIChat`, `OpenAIResponses`, `Anthropic`,
`Gemini`, `Bedrock`, `Ollama`, `GitHubCopilot`), an `AuthScheme`, and a
`Capabilities` bitset. `Catalog::merge_custom` performs an atomic, collision-
checked merge of user entries loaded by `custom::load`.

## Dependencies & features

Depends on `origin-core` (canonical `Message`/`Block` types), `origin-stream`
(the `Ring`), `origin-cas` (handle resolution), and `origin-multimodal`
(attachment blocks). Wire plumbing uses `reqwest` (rustls), `eventsource-stream`,
`async-stream`, `futures-util`, and `tokio-util`.

- **`recorder`** (optional): pulls in `origin-replay` and exposes the
  `recorder_hook` module — a global `ProviderTap` the daemon registers so concrete
  providers can feed frames through it without changing their public APIs.

## Used by

`Grep "origin-provider"` over `crates/*/Cargo.toml`:

```
crates/origin-cli/Cargo.toml
crates/origin-daemon/Cargo.toml
crates/origin-provider-anthropic/Cargo.toml
crates/origin-provider-bedrock/Cargo.toml
crates/origin-provider-gemini/Cargo.toml
crates/origin-provider-github/Cargo.toml
crates/origin-provider-ollama/Cargo.toml
crates/origin-provider-openai-compat/Cargo.toml
crates/origin-provider/Cargo.toml
crates/origin-sidecar/Cargo.toml
```

## Testing

Integration tests live in `tests/` (`trait.rs`, `handle_inflate.rs`) and exercise
the trait's default `chat_stream` and `inflate_tool_result_handles` semantics. The
`ReasoningEffort` wire-mapping has in-file unit tests under `effort_tests`, and
`catalog.rs` / `catalog_rows.rs` carry their own `#[cfg(test)]` modules.

## See also

- [Providers subsystem](../subsystems/providers.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
