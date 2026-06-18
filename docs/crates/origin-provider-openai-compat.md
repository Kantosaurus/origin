# origin-provider-openai-compat

> Generic OpenAI Chat-Completions wire client for OpenAI-shape providers

## Purpose

`origin-provider-openai-compat` is a single, parameterised [`Provider`]
implementation that serves *every* OpenAI-shape backend in the catalog. It is
parameterised over base URL, chat path, auth header, and extra headers, so one
client drives OpenAI, vLLM, Groq, Cerebras, Together, Mistral, DeepSeek, and
GitHub Copilot. It also ships a separate `OpenAiResponses` provider for the
distinct OpenAI **Responses API**, and a small `TokenSource` abstraction for
pluggable auth.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `OpenAiCompat` | struct | The Chat-Completions provider; `impl Provider`. |
| `OpenAiCompatConfig` | struct | `name`, `base_url`, `chat_path`, `auth: Arc<dyn TokenSource>`, `extra_headers`. |
| `OpenAiCompat::new` / `with_cas` | fn | Construct, and attach a CAS for handle inflation. |
| `OpenAiResponses` | struct | Provider for `POST /responses` (typed `input[]`/`output[]`). |
| `TokenSource` | trait | Async `header()` → `(name, value)`; pluggable auth. |
| `StaticBearer` | struct | `Authorization: Bearer <token>` token source. |
| `StaticHeader` | struct | Arbitrary fixed header token source. |
| `NoAuth` | struct | Sends no auth header (e.g. local servers). |
| `TokenError` | enum | Token-source failure type. |
| `streaming::parse_into_ring` | fn | Parse OpenAI SSE into a `Ring`, backend-aware. |

## Key types

```rust
#[derive(Clone)]
pub struct OpenAiCompatConfig {
    pub name: &'static str,
    pub base_url: String,
    pub chat_path: String,
    pub auth: Arc<dyn TokenSource>,
    pub extra_headers: Vec<(String, String)>,
}

pub struct OpenAiCompat { /* cfg, client, cas */ }

#[async_trait::async_trait]
pub trait TokenSource: Send + Sync + std::fmt::Debug {
    async fn header(&self) -> Result<(String, String), TokenError>;
}
```

## How it works

**Wire API.** `chat` encodes a typed `wire::WireRequest` to JSON, POSTs it to
`{base_url}{chat_path}` with `content-type: application/json`, and decodes the
`{choices[]}` response. `chat_stream` sets the streaming flag plus
`accept: text/event-stream` and pumps the SSE body through `parse_into_ring`.

**Auth.** Before sending, `auth.header().await` yields a `(header, value)` pair
applied to the request, followed by any `extra_headers`. `StaticBearer`,
`StaticHeader`, and `NoAuth` cover the common cases; GitHub Copilot supplies a
custom session-token `TokenSource`.

**Backend quirks.** The base URL is classified once via
`origin_shimquirks::Backend::from_base_url`. `apply_shim_quirks` remaps the model
alias and strips fields a backend cannot accept (`store`, `parallel_tool_calls`).
On decode, two backend-gated quirks fire for non-OpenAI backends only: raw-text
tool-call recovery (`parse_raw_toolcall_text` → a synthesised
`shimquirks_<name>` `Block::ToolUse`) and a `tracing::warn!` on length-limit
truncation. For `Backend::OpenAi` both are inert, so a canonical response decodes
byte-identically.

**CAS + attachments.** `inflate_tool_result_handles` resolves handle-backed tool
results to inline bytes; `append_attachments` promotes the last user message to
the OpenAI `content[]` parts shape and appends encoded image/text parts.

**Responses API.** `OpenAiResponses` exists because Codex/ChatGPT-OAuth providers
speak only `POST /responses` (typed `input[]` + `instructions`, returning
`output[]`), which 400s through the Chat-Completions path. Its streaming uses the
trait's default buffering `chat_stream`.

**Cassette tap.** `ORIGIN_CASSETTE=record|replay:<path>` records/replays both JSON
and SSE bodies, wrapping recorded SSE text back into a synthetic
`reqwest::Response` so it flows through the same parser.

## Dependencies & features

`origin-provider`, `origin-core`, `origin-shimquirks`, `origin-stream`,
`origin-cas`, `origin-cassette`, `origin-multimodal`; `reqwest` (rustls),
`http`, `futures-util`, `pin-utils`, `async-trait`, `serde`/`serde_json`. No
extra cargo features.

## Used by

`Grep "origin-provider-openai-compat"` over `crates/*/Cargo.toml`:

```
crates/origin-daemon/Cargo.toml
crates/origin-provider-github/Cargo.toml
crates/origin-provider-openai-compat/Cargo.toml
```

## Testing

`tests/` covers the wire shape and quirk wirings with `wiremock`: `wire_smoke.rs`,
`cassette.rs`, `cassette_stream.rs`, `attachments.rs`, `effort.rs`,
`thinking_tokens.rs`, `raw_toolcall_recovery.rs`, `streaming_tool_index.rs`,
`truncation_stream.rs`. In-file `decode_tests` assert the OpenAI-vs-non-OpenAI
no-op contract for raw-text recovery and truncation.

## See also

- [Providers subsystem](../subsystems/providers.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
