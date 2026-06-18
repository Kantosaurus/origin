# origin-provider-bedrock

> SigV4-signed AWS Bedrock provider for origin (Anthropic InvokeModel)

## Purpose

`origin-provider-bedrock` implements the [`Provider`] trait against AWS Bedrock's
`InvokeModel` endpoint, sending the Anthropic `bedrock-2023-05-31` request shape
and signing every request with AWS Signature Version 4. It is a non-streaming
provider in this phase — `chat_stream` falls back to the trait default. It owns
the SigV4 signing, the Anthropic-on-Bedrock wire encode/decode, and CAS handle
inflation.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Bedrock` | struct | The provider; `impl Provider`. |
| `Bedrock::new` | fn | Construct with endpoint, region, model id, and static AWS credentials. |
| `Bedrock::with_cas` | fn | Attach an `origin_cas::Store` for `ToolResult` handle inflation. |

## Key types

```rust
pub struct Bedrock {
    endpoint: String,
    region: String,
    model_id: String,
    access_key: String,
    secret_key: String,
    client: reqwest::Client,
    cas: Option<std::sync::Arc<origin_cas::Store>>,
}

impl Bedrock {
    pub fn new(
        endpoint: impl Into<String>,
        region: impl Into<String>,
        model_id: impl Into<String>,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Self;
    pub fn with_cas(self, cas: Arc<origin_cas::Store>) -> Self;
}
```

## How it works

**Wire API.** `chat` POSTs to `{endpoint}/model/{model_id}/invoke`. The body is the
Anthropic-shaped `WireRequest`: `anthropic_version = "bedrock-2023-05-31"`,
`max_tokens` (default 16,384), optional `system`, `messages[]`, and a top-level
`tools[]` array. Tool schemas must be declared at top level — without them the
model is never told tools exist, and a history containing `tool_use`/`tool_result`
blocks is rejected with a 400 for referencing undeclared tools.

**Auth (SigV4).** Before sending, `sigv4::signed_headers("POST", url, body_bytes,
region, access_key, secret_key)` computes the SigV4 signature for
`service = "bedrock"` and returns the headers (`Authorization`, `X-Amz-Date`, …)
to attach. Signing is over the exact serialized body bytes, which are then sent
as the request body.

**Decode.** The Anthropic-shape response (`content[]` of `text`/`tool_use`
blocks, `usage`) maps to a canonical `ChatResponse`; `tool_use` `input` is
re-serialized into `input_json`.

**CAS / messages.** `inflate_tool_result_handles` resolves handle-backed tool
results before encoding. `block_to_wire` maps canonical blocks to
`text`/`tool_use`/`tool_result`; `Thinking` blocks are dropped. Tool results and
the system prompt follow Anthropic-on-Bedrock conventions (tool results ride as
`user` messages; system text is the top-level `system` field).

**Errors.** `status_error` maps 401/403 → `Auth`, 429 → `RateLimit`, other →
`Api("status {s}: {body}")`.

## Dependencies & features

`origin-provider`, `origin-core`, `origin-stream`, `origin-cas`; `aws-sigv4` and
`aws-credential-types` for signing, `http`, `reqwest` (rustls, no `stream`
feature since streaming is unimplemented), `async-trait`, `serde`/`serde_json`.
No extra cargo features.

## Used by

`Grep "origin-provider-bedrock"` over `crates/*/Cargo.toml`:

```
crates/origin-daemon/Cargo.toml
crates/origin-provider-bedrock/Cargo.toml
```

## Testing

`tests/` covers signing and request shaping: `sigv4.rs` (signature determinism /
canonical request) and `max_tokens.rs`. The `src/sigv4.rs` module also carries its
own `#[cfg(test)]` coverage.

## See also

- [Providers subsystem](../subsystems/providers.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
