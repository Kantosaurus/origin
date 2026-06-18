# origin-provider-ollama

> Ollama provider for origin with NDJSON streaming chat

## Purpose

`origin-provider-ollama` implements the [`Provider`] trait against a local Ollama
daemon's `POST /api/chat` endpoint, with NDJSON (newline-delimited JSON) streaming.
Ollama is unauthenticated by default, so the provider holds only a base URL and an
HTTP client. It owns the Ollama wire encode/decode and tool-call surfacing.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Ollama` | struct | The provider; `impl Provider` + `Default`. |
| `Ollama::new` | fn | Construct against the default base (`http://127.0.0.1:11434`). |
| `Ollama::with_base_url` | fn | Point at an arbitrary base (testing / gateways). |
| `streaming::parse_into_ring` | fn | Parse an Ollama NDJSON response into a `Ring`. |

## Key types

```rust
pub struct Ollama { base: String, client: reqwest::Client }

impl Ollama {
    pub fn new() -> Self;                       // http://127.0.0.1:11434
    pub fn with_base_url(base: &str) -> Self;
}
impl Default for Ollama { /* = new() */ }

// streaming.rs
pub async fn parse_into_ring(resp: reqwest::Response, ring: &Ring)
    -> Result<(), ProviderError>;
```

## How it works

**Wire API.** Both `chat` and `chat_stream` POST to `{base}/api/chat` with
`content-type: application/json`. The body is built by
`wire::encode_request(&req, stream)` — the second argument toggles Ollama's
`"stream"` flag. The default base is `http://127.0.0.1:11434`.

**Auth.** None. Ollama runs locally and unauthenticated; the struct carries no
credentials.

**Streaming format.** Ollama emits NDJSON — one JSON `WireFrame` object per line,
not SSE. `streaming::parse_into_ring` reuses the shared `origin_provider::ndjson`
line splitter and publishes each frame's content delta into the `Ring`.

**Non-streaming decode.** A single `WireFrame` decodes to a `ChatResponse`. Text
content becomes a `Block::Text`. Tool calls carry no id from Ollama, so each is
synthesised as `call_<name>_<idx>` — the index disambiguates parallel same-name
calls for the agent loop's `tool_use`/`tool_result` matching. Usage maps
`prompt_eval_count` → `input_tokens` and `eval_count` → `output_tokens`.

**Errors.** `status_error` maps 401/403 → `Auth`, 429 → `RateLimit` (parsing
`retry-after`, default 1), other → `Api("status {s}: {body}")`.

## Dependencies & features

`origin-provider`, `origin-core`, `origin-multimodal`, `origin-stream`; `reqwest`
(rustls, `stream`), `futures-util`, `pin-utils`, `async-trait`,
`serde`/`serde_json`. No extra cargo features. Note this crate does not depend on
`origin-cas`: tool results are encoded directly without handle inflation.

## Used by

`Grep "origin-provider-ollama"` over `crates/*/Cargo.toml`:

```
crates/origin-daemon/Cargo.toml
crates/origin-provider-ollama/Cargo.toml
```

## Testing

`tests/round_trip.rs` exercises the wire encode/decode and NDJSON streaming with
`wiremock`.

## See also

- [Providers subsystem](../subsystems/providers.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
