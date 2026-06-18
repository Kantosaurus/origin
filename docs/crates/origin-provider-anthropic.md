# origin-provider-anthropic

> Anthropic Messages API provider for origin

## Purpose

`origin-provider-anthropic` implements the [`Provider`] trait against Anthropic's
Messages API (`POST /v1/messages`), in both non-streaming and SSE-streaming
modes. It owns the typed wire encode/decode, two auth paths (static API key and a
refreshed OAuth bearer with Claude-CLI impersonation headers), prompt-cache
`cache_control` marker placement driven by an `origin-planner` `Plan`, extended
thinking, reasoning effort, and multimodal attachment injection.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Anthropic` | struct | The provider; `impl Provider`. |
| `Anthropic::new` | fn | Construct with the default base URL and an API key. |
| `Anthropic::with_base_url` / `with_endpoint` / `with_base` | fn | Point at an arbitrary base (testing / proxies). |
| `Anthropic::with_oauth_bearer` | fn | Use a refreshed OAuth token + Claude-CLI headers instead of `x-api-key`. |
| `Anthropic::with_cas` | fn | Attach an `origin_cas::Store` for `ToolResult` handle inflation. |
| `Anthropic::with_plan` | fn | Attach an `origin_planner::Plan` for cache-marker placement. |
| `streaming::parse_into_ring` | fn | Parse an Anthropic SSE byte stream into a `Ring`. |
| `encode_request_for_test` | fn | `#[doc(hidden)]` test helper returning the JSON request body. |

## Key types

```rust
pub struct Anthropic { /* auth, base, client, cas, plan, oauth metadata */ }

impl Anthropic {
    pub fn new(api_key: impl Into<String>) -> Self;
    pub fn with_oauth_bearer(token: impl Into<String>) -> Self;
    pub fn with_cas(self, cas: std::sync::Arc<origin_cas::Store>) -> Self;
    pub fn with_plan(self, plan: origin_planner::Plan) -> Self;
}

// streaming.rs
pub async fn parse_into_ring<R>(reader: R, ring: &Ring) -> Result<(), StreamingError>
where R: tokio::io::AsyncRead + Unpin;
```

## How it works

**Wire API.** Requests POST a typed `WireRequest` to `{base}/v1/messages` carrying
`model`, `max_tokens`, optional `system`, `messages[]`, `tools[]`, and (when set)
`output_config.effort` and a `thinking` block. `API_VERSION` is `2023-06-01`;
`DEFAULT_MAX_TOKENS` is 16,384. Streaming sets `"stream": true` and
`accept: text/event-stream`.

**Auth.** `AuthKind::ApiKey` sends `x-api-key: <key>`. `AuthKind::OAuthBearer`
sends `Authorization: Bearer <token>` plus a full Claude-CLI impersonation header
set (`anthropic-beta`, `User-Agent`, `X-Stainless-*`, session id), hits
`/v1/messages?beta=true`, and injects an `x-anthropic-billing-header` prefix into
the system prompt. OAuth metadata (`device_id`/`account_uuid`) is loaded from
`~/.claude.json`.

**Streaming format.** The response is Anthropic SSE
(`message_start`/`content_block_delta`/`message_delta`/…). `parse_into_ring`
consumes the byte stream and publishes deltas into the `Ring`.

**Cache markers.** `message_to_wire` plants `cache_control: {"type":"ephemeral"}`
on a block via three paths: planner `marker_indices` on message 0, an in-band
`cache_marker` on any block, or a dynamic per-turn marker on the last emitting
block. A `Frozen` boundary can opt into the 1-hour TTL via `ORIGIN_CACHE_TTL_1H=1`.
A warning fires past Anthropic's 4-marker-per-request ceiling.

**Thinking.** `thinking_tokens` enables extended thinking; models tagged
`-4-6`/`-4-7`/`-4-8` use `{"type":"adaptive"}` (manual `budget_tokens` 400s on
4.7+), older models use `{"type":"enabled","budget_tokens":n}`, and `max_tokens`
is bumped above the budget.

**Tool results / CAS.** `expand_messages_for_wire` resolves CAS handles, consults
the `Plan`'s per-handle band to decide inline-vs-`<result handle:… — N bytes>`
reference, and strips orphan `tool_result` blocks before the API rejects them.

**Cassette tap.** `ORIGIN_CASSETTE=record:<path>|replay:<path>` records/replays
both `chat` JSON and `chat_stream` SSE bodies (secret-scrubbed, save-gated).

## Dependencies & features

`origin-provider`, `origin-core`, `origin-planner`, `origin-stream`, `origin-cas`,
`origin-cassette`, `origin-multimodal`; `reqwest` (rustls), `eventsource-stream`,
`futures-util`, `pin-utils`, `tokio-util`, `uuid` (v4/v5 for session/device ids),
`dirs` (for `~/.claude.json`), `serde`/`serde_json`. No cargo features beyond the
workspace defaults.

## Used by

`Grep "origin-provider-anthropic"` over `crates/*/Cargo.toml`:

```
crates/origin-daemon/Cargo.toml
crates/origin-provider-anthropic/Cargo.toml
```

## Testing

A large `tests/` suite drives the wire encode/decode and streaming end-to-end with
`wiremock` and recorded fixtures: `messages.rs`, `streaming.rs`,
`streaming_e2e.rs`, `streaming_index.rs`, `cache_markers.rs`,
`cache_marker_multi_msg.rs`, `thinking_tokens.rs`, `effort.rs`, `max_tokens.rs`,
`attachments.rs`, `handle_substitution.rs`, `cassette.rs`, `cassette_stream.rs`,
plus a `fixtures/` directory.

## See also

- [Providers subsystem](../subsystems/providers.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
