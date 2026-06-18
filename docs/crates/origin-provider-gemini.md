# origin-provider-gemini

> Google Gemini generateContent provider for origin, with SSE streaming

## Purpose

`origin-provider-gemini` implements the [`Provider`] trait against Google's
Generative Language REST API: `generateContent` for non-streaming turns and
`streamGenerateContent` (SSE) for streaming. It owns the typed Gemini wire
encode/decode (`contents[]`/`parts[]`/`functionCall`/`functionResponse`), two
auth paths (API-key query parameter and OAuth bearer), function-call id packing,
and CAS handle inflation.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Gemini` | struct | The provider; `impl Provider`. |
| `Gemini::new` | fn | Construct with the default base URL and an API key. |
| `Gemini::with_base_url` | fn | Point at an arbitrary base (testing / gateways). |
| `Gemini::with_oauth_bearer` | fn | Send `Authorization: Bearer <token>`, omit `?key=`. |
| `Gemini::with_cas` | fn | Attach an `origin_cas::Store` for `ToolResult` handle inflation. |
| `streaming::parse_into_ring` | fn | Parse a Gemini SSE response into a `Ring`. |

## Key types

```rust
pub struct Gemini { /* auth, base, client, cas */ }   // deliberately no Debug (no key leak)

impl Gemini {
    pub fn new(api_key: impl Into<String>) -> Self;
    pub fn with_oauth_bearer(token: impl Into<String>) -> Self;
    pub fn with_cas(self, cas: std::sync::Arc<origin_cas::Store>) -> Self;
}

// streaming.rs
pub async fn parse_into_ring(resp: reqwest::Response, ring: &Ring)
    -> Result<(), ProviderError>;
```

## How it works

**Wire API.** Non-streaming POSTs to
`{base}/v1beta/models/{model}:generateContent`, streaming to
`{model}:streamGenerateContent` with `&alt=sse` and `accept: text/event-stream`.
The base defaults to `https://generativelanguage.googleapis.com`. The request
body is built by `wire::encode_request`; the response is decoded from
`candidates[].content.parts[]`.

**Auth.** `AuthKind::ApiKey` embeds the key as a `?key=<api_key>` query parameter
(not a header) and applies no auth header. `AuthKind::OAuthBearer` instead sends
`Authorization: Bearer <token>` and drops the `?key=` parameter, rewriting the
query string so `&alt=sse` becomes the first `?alt=sse` parameter. The struct does
not derive `Debug`, so the key cannot be logged.

**Streaming format.** Gemini SSE; `streaming::parse_into_ring` reuses the shared
`origin_provider::sse` pump and publishes text/function-call deltas into the
`Ring`.

**Function calls.** On decode, a `functionCall` part becomes a `Block::ToolUse`.
Gemini's real call id is packed as `name#id` so the tool-result encoder can echo
it back (correct mapping for parallel same-name calls); when absent the id falls
back to `call_<name>`.

**Usage.** `usageMetadata` maps to canonical `Usage`, including
`cachedContentTokenCount` → `cache_read_input_tokens`.

**Errors.** `status_error` maps 401/403 → `Auth`, 429 → `RateLimit` (parsing
`retry-after`, default 1), other → `Api("status {s}: {body}")`.

## Dependencies & features

`origin-provider`, `origin-core`, `origin-multimodal`, `origin-stream`,
`origin-cas`; `reqwest` (rustls), `futures-util`, `pin-utils`, `async-trait`,
`serde`/`serde_json`. No extra cargo features.

## Used by

`Grep "origin-provider-gemini"` over `crates/*/Cargo.toml`:

```
crates/origin-daemon/Cargo.toml
crates/origin-provider-gemini/Cargo.toml
```

## Testing

`tests/` exercises the wire round-trip and SSE handling with `wiremock`:
`wire_round_trip.rs`, `streaming.rs`, `streaming_usage_final.rs`, and
`streaming_dup_function_ids.rs` (covering the `name#id` packing for duplicate
function ids).

## See also

- [Providers subsystem](../subsystems/providers.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
