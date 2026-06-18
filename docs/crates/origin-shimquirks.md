# origin-shimquirks

> OpenAI-compatibility shim per-backend request/response quirk handling

## Purpose

`origin-shimquirks` is pure request/response massaging so that one
OpenAI-compatible client (`origin-provider-openai-compat`) can talk to many subtly
different backends without panicking on their quirks. It classifies a backend from
its base URL, strips request fields a backend rejects, remaps model aliases,
recovers raw-text tool calls, detects truncation, and redacts secrets from URLs.
It has no network and no async — every function is a pure transform.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Backend` | enum | Known OpenAI-compat flavors: `OpenAi`, `VLlm`, `Cerebras`, `Groq`, `Together`, `Ollama`, `Mistral`, `DeepSeek`, `Other`. |
| `Backend::from_base_url` / `from_base_url` | fn | Classify a backend from its base URL (host-substring, case-insensitive). |
| `apply_request_quirks` | fn | Mutate a request body in place: drop `store` / `parallel_tool_calls` per backend. |
| `map_model_name` | fn | Map a canonical model id to the alias a backend expects (identity otherwise). |
| `redact_url_secrets` | fn | Scrub credential query params and inline `user:pass@` userinfo from a URL. |
| `detect_truncation` | fn | `true` for finish reasons `length` / `max_tokens`. |
| `parse_raw_toolcall_text` | fn | Recover an inline tool call (`<tool_call>` tag, fenced JSON, or bare object). |
| `ShimError` | enum | `Json(String)` parse/shape error. |

## Key types

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend { OpenAi, VLlm, Cerebras, Groq, Together, Ollama, Mistral, DeepSeek, Other }

impl Backend {
    pub fn from_base_url(url: &str) -> Self;
}

pub fn apply_request_quirks(backend: Backend, body: &mut serde_json::Value);
pub fn map_model_name(backend: Backend, model: &str) -> String;
pub fn redact_url_secrets(url: &str) -> String;
pub fn detect_truncation(finish_reason: Option<&str>) -> bool;
pub fn parse_raw_toolcall_text(text: &str) -> Option<(String, String)>;
```

## How it works

**Classification.** `from_base_url` lowercases the URL and matches host
substrings, checking specific vendors before generic fallbacks
(`cerebras.ai`/`groq.com`/`together.ai`/`mistral.ai`/`deepseek.com`, then
`11434`/`ollama`, then `vllm`/`8000`, then `openai.com`). Unknown hosts →
`Backend::Other`.

**Request quirks.** `apply_request_quirks` is best-effort and never panics: a
non-object body is left alone. vLLM and Cerebras reject a top-level `store`, so it
is removed; vLLM/Cerebras/Groq/Together/Ollama/DeepSeek do not understand
`parallel_tool_calls`, so it is dropped. OpenAI/Mistral/Other are untouched.

**Model aliases.** `map_model_name` carries small per-backend alias tables
(e.g. Groq `llama-3.1-70b` → `llama-3.1-70b-versatile`, Cerebras `llama3.1-8b`,
Ollama `llama3.1:8b`); unknown models echo through unchanged.

**Raw-text tool calls.** Some backends emit a tool call as assistant text rather
than a structured `tool_calls` array. `parse_raw_toolcall_text` extracts the JSON
body from a `<tool_call>…</tool_call>` tag, a fenced code block, or the first
balanced top-level `{…}` object, then returns `(name, arguments_json)`.

**Secret redaction.** `redact_url_secrets` preserves URL structure while replacing
secret query-param values (`api_key`, `apikey`, `key`, `token`, `access_token`,
`auth`) and inline userinfo with `***`, so endpoints are safe to log in errors.

## Dependencies & features

Only `serde_json` and `thiserror`. No async, no network, no other origin crates.
No cargo features.

## Used by

`Grep "origin-shimquirks"` over `crates/*/Cargo.toml`:

```
crates/origin-provider-openai-compat/Cargo.toml
crates/origin-shimquirks/Cargo.toml
```

## Testing

All coverage is in-file (`#[cfg(test)] mod tests`): host classification, the
`store`/`parallel_tool_calls` quirk matrix, secret redaction (api_key + userinfo +
token), truncation detection, the tag/fenced/string forms of raw tool-call
recovery, model-name aliasing, and the `ShimError` display.

## See also

- [Providers subsystem](../subsystems/providers.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
