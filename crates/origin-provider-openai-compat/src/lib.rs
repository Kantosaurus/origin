// SPDX-License-Identifier: Apache-2.0
//! Generic `OpenAI` Chat-Completions wire client.
//!
//! Parameterised over base URL, chat path, auth header, and extra headers so
//! it can serve every OpenAI-shape provider in the catalog from a single
//! implementation.

pub mod streaming;
mod token_source;
pub mod wire;

pub use token_source::{NoAuth, StaticBearer, StaticHeader, TokenSource};

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_provider::openai_tools::tool_call_to_block;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use origin_shimquirks::Backend;
use reqwest::StatusCode;
use std::sync::Arc;

#[derive(Clone)]
pub struct OpenAiCompatConfig {
    pub name: &'static str,
    pub base_url: String,
    pub chat_path: String,
    pub auth: Arc<dyn TokenSource>,
    pub extra_headers: Vec<(String, String)>,
}

pub struct OpenAiCompat {
    cfg: OpenAiCompatConfig,
    client: reqwest::Client,
    cas: Option<Arc<origin_cas::Store>>,
}

impl OpenAiCompat {
    #[must_use]
    pub fn new(cfg: OpenAiCompatConfig) -> Self {
        Self {
            cfg,
            client: reqwest::Client::new(),
            cas: None,
        }
    }

    /// Attach the content-addressed store so handle-backed `ToolResult` blocks
    /// (produced by the daemon whenever a CAS is configured) are inflated to
    /// inline bytes before wire encoding. Without this, follow-up turns send
    /// empty tool results and the agentic loop silently breaks.
    #[must_use]
    pub fn with_cas(mut self, cas: Arc<origin_cas::Store>) -> Self {
        self.cas = Some(cas);
        self
    }
}

#[async_trait]
impl Provider for OpenAiCompat {
    fn name(&self) -> &'static str {
        self.cfg.name
    }

    fn base_url(&self) -> Option<&str> {
        Some(&self.cfg.base_url)
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let messages = origin_provider::inflate_tool_result_handles(&req.messages, self.cas.as_ref())?;
        let req = ChatRequest { messages, ..req };
        // Encode the typed wire request, then drop to a JSON value so the
        // backend-specific quirk pass can remap the model alias and strip fields
        // the detected backend cannot accept (e.g. `store` for vLLM/Cerebras).
        // A no-op for OpenAi/Other, so canonical OpenAI requests are unchanged.
        let mut body = serde_json::to_value(wire::encode_request(&req, false))
            .map_err(|e| ProviderError::Api(format!("encode: {e}")))?;
        // Inject multimodal attachments (item G-live). No-op when empty, so the
        // default text-only request is byte-identical (item G-live).
        append_attachments(&mut body, &req.attachments);
        apply_shim_quirks(&self.cfg.base_url, &mut body);
        // Classify the backend once; the response decode path uses it to (a)
        // recover a raw-text tool call some non-OpenAi backends emit instead of a
        // structured `tool_calls` array and (b) recognise a length-limit
        // truncation. Both are no-ops for canonical OpenAI responses.
        let backend = origin_shimquirks::Backend::from_base_url(&self.cfg.base_url);
        let url = format!(
            "{}{}",
            self.cfg.base_url.trim_end_matches('/'),
            self.cfg.chat_path
        );

        // Optional cassette tap on the NON-STREAMING path (env
        // `ORIGIN_CASSETTE=record:<path>|replay:<path>`). Default (unset) returns
        // `None`, so the network path below is unchanged and byte-identical to
        // the pre-cassette behavior. Mirrors the streaming tap above and the
        // Anthropic non-streaming `chat()` tap: replay serves the recorded JSON
        // body with zero network I/O; record scrubs secrets + is save-gated.
        let cassette_mode = cassette::Mode::from_env();
        let req_body_text = serde_json::to_string(&body).unwrap_or_default();

        // Replay mode: serve the recorded response body from disk with no network
        // I/O, decoding it through the same `wire::WireResponse` → `decode_response`
        // path a live response uses.
        if let Some(cassette::Mode::Replay(path)) = &cassette_mode {
            let text = cassette::replay(path, "POST", &url, &req_body_text)?;
            let wire: wire::WireResponse = serde_json::from_str(&text)
                .map_err(|e| ProviderError::Api(format!("cassette decode: {e}")))?;
            return Ok(decode_response(wire, backend));
        }

        let mut builder = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body);
        let (hdr, val) = self.cfg.auth.header().await.map_err(|_| ProviderError::Auth)?;
        if !hdr.is_empty() {
            builder = builder.header(hdr, val);
        }
        for (h, v) in &self.cfg.extra_headers {
            builder = builder.header(h, v);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        if resp.status() == StatusCode::OK {
            // Record mode: capture the raw body text so it can be replayed later
            // (after secret scrubbing + the save gate), then decode it; otherwise
            // decode the response directly.
            if let Some(cassette::Mode::Record(path)) = &cassette_mode {
                let text = resp
                    .text()
                    .await
                    .map_err(|e| ProviderError::Api(format!("decode: {e}")))?;
                cassette::record(path, "POST", &url, &req_body_text, 200, &text)?;
                let wire: wire::WireResponse =
                    serde_json::from_str(&text).map_err(|e| ProviderError::Api(format!("decode: {e}")))?;
                return Ok(decode_response(wire, backend));
            }
            let wire: wire::WireResponse = resp
                .json()
                .await
                .map_err(|e| ProviderError::Api(format!("decode: {e}")))?;
            return Ok(decode_response(wire, backend));
        }
        Err(status_error(resp, &url).await)
    }

    async fn chat_stream(&self, req: ChatRequest, ring: &origin_stream::Ring) -> Result<(), ProviderError> {
        let messages = origin_provider::inflate_tool_result_handles(&req.messages, self.cas.as_ref())?;
        let req = ChatRequest { messages, ..req };
        let mut body = serde_json::to_value(wire::encode_request(&req, true))
            .map_err(|e| ProviderError::Api(format!("encode: {e}")))?;
        // Inject multimodal attachments before the quirk pass, mirroring `chat`.
        // Streaming is the DEFAULT provider path, so omitting this silently
        // dropped images/PDFs on every streamed turn. No-op when empty ⇒
        // byte-identical to the text-only request.
        append_attachments(&mut body, &req.attachments);
        apply_shim_quirks(&self.cfg.base_url, &mut body);
        // Classify the backend once so the streaming finalize path can recognise
        // a length-limit truncation. A no-op for canonical OpenAI backends.
        let backend = origin_shimquirks::Backend::from_base_url(&self.cfg.base_url);
        let url = format!(
            "{}{}",
            self.cfg.base_url.trim_end_matches('/'),
            self.cfg.chat_path
        );

        // Optional cassette tap on the STREAMING path (env
        // `ORIGIN_CASSETTE=record:<path>|replay:<path>`). Default (unset) returns
        // `None`, so the network path below is unchanged and byte-identical to
        // the pre-cassette behavior. The recorded body is the raw SSE event-stream
        // text; on both record and replay it is wrapped back into a synthetic
        // `reqwest::Response` so it flows through the exact same SSE → ring parser
        // a live response would.
        let cassette_mode = cassette::Mode::from_env();
        let req_body_text = serde_json::to_string(&body).unwrap_or_default();

        // Replay mode: serve the recorded SSE text from disk with no network I/O.
        if let Some(cassette::Mode::Replay(path)) = &cassette_mode {
            let sse = cassette::replay(path, "POST", &url, &req_body_text)?;
            let result = streaming::parse_into_ring(response_from_sse(sse), ring, backend).await;
            ring.close();
            return result;
        }

        let mut builder = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&body);
        let (hdr, val) = self.cfg.auth.header().await.map_err(|_| ProviderError::Auth)?;
        if !hdr.is_empty() {
            builder = builder.header(hdr, val);
        }
        for (h, v) in &self.cfg.extra_headers {
            builder = builder.header(h, v);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        if !resp.status().is_success() {
            let err = status_error(resp, &url).await;
            ring.close();
            return Err(err);
        }

        // Record mode: buffer the whole SSE body to text, persist it (after
        // secret scrubbing + the save gate), THEN replay the buffered text into
        // the ring so the live caller still streams. Buffering is acceptable here
        // because recording is a test/dev affordance, not the hot path.
        if let Some(cassette::Mode::Record(path)) = &cassette_mode {
            let sse = match resp.text().await {
                Ok(t) => t,
                Err(e) => {
                    ring.close();
                    return Err(ProviderError::Api(format!("stream decode: {e}")));
                }
            };
            cassette::record(path, "POST", &url, &req_body_text, 200, &sse)?;
            let result = streaming::parse_into_ring(response_from_sse(sse), ring, backend).await;
            ring.close();
            return result;
        }

        let result = streaming::parse_into_ring(resp, ring, backend).await;
        ring.close();
        result
    }
}

/// Wrap recorded SSE event-stream text into a synthetic `reqwest::Response`.
///
/// The cassette stores the raw SSE body as UTF-8 text; replaying (and the
/// record-then-feed path) rebuilds a `200 OK` `reqwest::Response` carrying that
/// text so it can flow through the same [`streaming::parse_into_ring`] adapter a
/// live network response uses — no second SSE parser, no server.
fn response_from_sse(sse: String) -> reqwest::Response {
    // `http::Response<String>` → `reqwest::Response` (no network); `String`
    // satisfies `Into<reqwest::Body>`. This never fails for an in-memory body.
    reqwest::Response::from(http::Response::new(sse))
}

/// Cassette tap on the streaming `chat_stream()` path (workstream WS-S).
///
/// Mirrors the non-streaming tap shipped on the Anthropic provider: records
/// every streamed request + raw SSE body into an `origin-cassette` file when
/// `ORIGIN_CASSETTE=record:<path>` is set (scrubbing secrets before persisting),
/// and serves the recorded SSE without any network I/O when
/// `ORIGIN_CASSETTE=replay:<path>` is set. With the variable unset, nothing in
/// this module runs and the stream path is byte-identical to before.
mod cassette {
    use origin_cassette::{Cassette, Interaction, ReqShape, RespShape};
    use origin_provider::ProviderError;

    /// Parsed `ORIGIN_CASSETTE` mode. The path is the cassette JSON file.
    pub enum Mode {
        /// `record:<path>` — append each interaction to the cassette on disk.
        Record(String),
        /// `replay:<path>` — serve recorded responses, no network call.
        Replay(String),
    }

    impl Mode {
        /// Parse the `ORIGIN_CASSETTE` env var.
        ///
        /// Returns `None` when unset or malformed, so the default (no-cassette)
        /// path is unaffected.
        #[must_use]
        pub fn from_env() -> Option<Self> {
            let raw = std::env::var("ORIGIN_CASSETTE").ok()?;
            if let Some(p) = raw.strip_prefix("record:") {
                return (!p.is_empty()).then(|| Self::Record(p.to_string()));
            }
            if let Some(p) = raw.strip_prefix("replay:") {
                return (!p.is_empty()).then(|| Self::Replay(p.to_string()));
            }
            None
        }
    }

    /// Append a streamed request/SSE-body interaction to the cassette at `path`.
    ///
    /// Scrubs secrets before persisting and refuses to save if any leak remains.
    ///
    /// # Errors
    /// Returns [`ProviderError::Api`] if the existing cassette cannot be parsed,
    /// if a secret survives scrubbing, or if the file cannot be written.
    pub fn record(
        path: &str,
        method: &str,
        url: &str,
        req_body: &str,
        status: u16,
        resp_body: &str,
    ) -> Result<(), ProviderError> {
        let mut cassette = match std::fs::read_to_string(path) {
            Ok(existing) => Cassette::from_json(&existing)
                .map_err(|e| ProviderError::Api(format!("cassette parse: {e}")))?,
            Err(_) => Cassette::new("openai-compat-stream"),
        };
        cassette.record(Interaction {
            request: ReqShape {
                method: method.to_string(),
                url: url.to_string(),
                headers: Vec::new(),
                body: req_body.to_string(),
            },
            response: RespShape {
                status,
                headers: Vec::new(),
                body: resp_body.to_string(),
            },
        });
        // Scrub credentials, then hard-gate the save so a live token can never be
        // persisted to a cassette file.
        origin_cassette::scrub_secrets(&mut cassette);
        origin_cassette::assert_redacted(&cassette)
            .map_err(|e| ProviderError::Api(format!("cassette redaction gate: {e}")))?;
        let json = cassette
            .to_json()
            .map_err(|e| ProviderError::Api(format!("cassette serialize: {e}")))?;
        std::fs::write(path, json).map_err(|e| ProviderError::Api(format!("cassette write: {e}")))
    }

    /// Replay the recorded SSE body for a `(method, url)` request from the
    /// cassette at `path`.
    ///
    /// # Errors
    /// Returns [`ProviderError::Api`] if the cassette cannot be read/parsed, no
    /// matching interaction exists, or the recorded status is non-OK.
    pub fn replay(path: &str, method: &str, url: &str, req_body: &str) -> Result<String, ProviderError> {
        let text =
            std::fs::read_to_string(path).map_err(|e| ProviderError::Api(format!("cassette read: {e}")))?;
        let cassette =
            Cassette::from_json(&text).map_err(|e| ProviderError::Api(format!("cassette parse: {e}")))?;
        let probe = ReqShape {
            method: method.to_string(),
            url: url.to_string(),
            headers: Vec::new(),
            body: req_body.to_string(),
        };
        let interaction = cassette
            .match_next(&probe)
            .ok_or_else(|| ProviderError::Api(format!("cassette replay miss for {method} {url}")))?;
        if interaction.response.status != 200 {
            return Err(ProviderError::Api(format!(
                "cassette replay status {}",
                interaction.response.status
            )));
        }
        Ok(interaction.response.body.clone())
    }
}

async fn status_error(resp: reqwest::Response, url: &str) -> ProviderError {
    let status = resp.status();
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProviderError::Auth,
        StatusCode::TOO_MANY_REQUESTS => {
            let retry = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(1);
            ProviderError::RateLimit {
                retry_after_secs: retry,
                message: String::new(),
            }
        }
        s => {
            let body = resp.text().await.unwrap_or_default();
            // Redact any credential query params / inline userinfo from the
            // endpoint before it lands in an error string a caller may log.
            let safe_url = origin_shimquirks::redact_url_secrets(url);
            ProviderError::Api(format!("status {s} ({safe_url}): {body}"))
        }
    }
}

/// Append multimodal attachments to the last user message (item G-live).
///
/// `OpenAI` multimodal messages carry `content` as an array of typed parts. When
/// attachments are present, the last `{"role":"user"}` message's string content
/// is promoted to a `[{"type":"text",...}]` array and the encoded image/text
/// parts (via [`origin_multimodal::encode_openai_block`]) are appended. A no-op
/// when `attachments` is empty, so the canonical text request is byte-identical.
/// When there is no user message, a fresh one carrying the parts is appended.
fn append_attachments(body: &mut serde_json::Value, attachments: &[origin_multimodal::ContentBlock]) {
    if attachments.is_empty() {
        return;
    }
    let encoded: Vec<serde_json::Value> = attachments
        .iter()
        .map(origin_multimodal::encode_openai_block)
        .collect();
    let Some(messages) = body.get_mut("messages").and_then(serde_json::Value::as_array_mut) else {
        return;
    };
    let last_user = messages
        .iter_mut()
        .rev()
        .find(|m| m.get("role").and_then(serde_json::Value::as_str) == Some("user"));
    if let Some(msg) = last_user {
        // Promote existing string content to a text part, then append the
        // encoded attachment parts.
        let mut parts: Vec<serde_json::Value> = match msg.get("content") {
            Some(serde_json::Value::String(s)) => {
                vec![serde_json::json!({ "type": "text", "text": s })]
            }
            Some(serde_json::Value::Array(arr)) => arr.clone(),
            _ => Vec::new(),
        };
        parts.extend(encoded);
        if let Some(obj) = msg.as_object_mut() {
            obj.insert("content".to_string(), serde_json::Value::Array(parts));
        }
        return;
    }
    messages.push(serde_json::json!({ "role": "user", "content": encoded }));
}

/// Apply backend-specific request quirks to an already-encoded request body.
///
/// Detects the concrete backend from `base_url`, remaps the `model` field to the
/// alias that backend expects, then strips fields the backend does not
/// understand via [`origin_shimquirks::apply_request_quirks`]. For
/// [`origin_shimquirks::Backend::OpenAi`] and `Backend::Other` this is a no-op,
/// so a canonical `OpenAI` request is left byte-identical.
fn apply_shim_quirks(base_url: &str, body: &mut serde_json::Value) {
    let backend = origin_shimquirks::Backend::from_base_url(base_url);
    if let Some(model) = body.get("model").and_then(serde_json::Value::as_str) {
        let mapped = origin_shimquirks::map_model_name(backend, model);
        if mapped != model {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("model".to_string(), serde_json::Value::String(mapped));
            }
        }
    }
    origin_shimquirks::apply_request_quirks(backend, body);
}

/// Decode a non-streaming `OpenAI`-shape response into a canonical
/// [`ChatResponse`], applying two backend-gated shim quirks.
///
/// 1. **Raw-text tool-call recovery.** When the structured `tool_calls` field is
///    empty/absent BUT the assistant text contains an inline tool call (some
///    non-OpenAi backends — e.g. raw Gemini-via-compat — emit the call as text),
///    [`origin_shimquirks::parse_raw_toolcall_text`] recovers a proper
///    [`Block::ToolUse`] and the raw text is dropped in its favour. Skipped when
///    structured `tool_calls` are present or for [`Backend::OpenAi`].
/// 2. **Truncation diagnostic.** A `length`/`max_tokens` finish on a non-OpenAi
///    backend (see [`origin_shimquirks::detect_truncation`]) emits a
///    `tracing::warn!`, since the canonical [`ChatResponse`] has no truncation
///    field. A no-op for [`Backend::OpenAi`].
///
/// For [`Backend::OpenAi`] both quirks are deliberately inert, so a well-behaved
/// `OpenAI` response decodes byte-identically to before.
fn decode_response(wire: wire::WireResponse, backend: Backend) -> ChatResponse {
    let mut blocks: Vec<Block> = Vec::new();
    if let Some(choice) = wire.choices.into_iter().next() {
        // Surface a length-limit truncation as a diagnostic for quirky backends.
        if backend != Backend::OpenAi && origin_shimquirks::detect_truncation(choice.finish_reason.as_deref())
        {
            tracing::warn!(
                finish_reason = choice.finish_reason.as_deref().unwrap_or_default(),
                ?backend,
                "completion truncated at a length limit; output is incomplete"
            );
        }

        let text = choice.message.content;
        let has_structured_tool_calls = choice
            .message
            .tool_calls
            .as_ref()
            .is_some_and(|tc| !tc.is_empty());

        // Fall back to recovering a raw inline tool call ONLY when the backend is
        // non-OpenAi, no structured tool call was provided, and the assistant
        // text actually parses as one. Otherwise keep the text block verbatim.
        let recovered = if backend != Backend::OpenAi && !has_structured_tool_calls {
            text.as_deref()
                .and_then(origin_shimquirks::parse_raw_toolcall_text)
        } else {
            None
        };

        if let Some((name, arguments)) = recovered {
            blocks.push(Block::ToolUse {
                // Backends that emit raw-text tool calls do not supply a call id;
                // synthesise a stable, namespaced one so the agentic loop can
                // still pair the eventual tool result back to this call.
                id: format!("shimquirks_{name}"),
                name,
                input_json: arguments.into_bytes(),
                cache_marker: None,
            });
        } else if let Some(text) = text {
            if !text.is_empty() {
                blocks.push(Block::Text {
                    text,
                    cache_marker: None,
                });
            }
        }

        if let Some(tool_calls) = choice.message.tool_calls {
            for tc in &tool_calls {
                blocks.push(tool_call_to_block(tc));
            }
        }
    }
    ChatResponse {
        assistant: Message {
            role: Role::Assistant,
            blocks,
        },
        usage: Usage {
            input_tokens: wire.usage.prompt_tokens,
            output_tokens: wire.usage.completion_tokens,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod decode_tests {
    //! Unit coverage for the two shimquirks response-path wirings: raw-text
    //! tool-call recovery and the `OpenAi` vs non-`OpenAi` no-op contract. The
    //! wire response types are `Deserialize`-only, so each fixture is built from
    //! JSON.

    use super::{decode_response, wire, Backend, Block};

    fn wire_from(json: serde_json::Value) -> wire::WireResponse {
        serde_json::from_value(json).expect("fixture is a valid WireResponse")
    }

    #[test]
    fn recovers_raw_text_tool_call_for_quirky_backend() {
        // Some non-OpenAi backends emit the tool call inline as assistant text
        // (e.g. raw Gemini-via-compat) with no structured `tool_calls`.
        let wire = wire_from(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "<tool_call>{\"name\": \"get_weather\", \"arguments\": {\"city\": \"NYC\"}}</tool_call>"
                },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 3, "completion_tokens": 7 }
        }));

        let resp = decode_response(wire, Backend::Other);
        assert_eq!(resp.assistant.blocks.len(), 1, "raw text replaced by tool use");
        match &resp.assistant.blocks[0] {
            Block::ToolUse {
                id, name, input_json, ..
            } => {
                assert_eq!(name, "get_weather");
                assert_eq!(id, "shimquirks_get_weather");
                let args: serde_json::Value = serde_json::from_slice(input_json).unwrap();
                assert_eq!(args, serde_json::json!({ "city": "NYC" }));
            }
            other => panic!("expected recovered ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn openai_backend_never_recovers_raw_text_tool_call() {
        // Identical body, but a canonical OpenAI backend must leave the text
        // verbatim (no recovery) — byte-identical decode to before this wiring.
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "<tool_call>{\"name\": \"get_weather\", \"arguments\": {\"city\": \"NYC\"}}</tool_call>"
                },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 3, "completion_tokens": 7 }
        });
        let resp = decode_response(wire_from(body), Backend::OpenAi);
        assert_eq!(resp.assistant.blocks.len(), 1);
        match &resp.assistant.blocks[0] {
            Block::Text { text, .. } => assert!(text.contains("<tool_call>")),
            other => panic!("OpenAi must keep raw text verbatim, got {other:?}"),
        }
    }

    #[test]
    fn structured_tool_calls_suppress_raw_recovery() {
        // When structured tool_calls are present, the raw-text fallback is a
        // no-op even for a quirky backend: the text stays a text block and the
        // structured call is decoded normally.
        let wire = wire_from(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "here you go {\"name\":\"noise\"}",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "real_tool", "arguments": "{\"k\":1}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
        }));
        let resp = decode_response(wire, Backend::Other);
        // text block + the one structured tool call (raw fallback skipped).
        assert_eq!(resp.assistant.blocks.len(), 2);
        assert!(matches!(resp.assistant.blocks[0], Block::Text { .. }));
        match &resp.assistant.blocks[1] {
            Block::ToolUse { id, name, .. } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "real_tool");
            }
            other => panic!("expected structured ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn clean_openai_text_response_is_byte_identical() {
        let wire = wire_from(serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": "hello world" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 2 }
        }));
        let resp = decode_response(wire, Backend::OpenAi);
        assert_eq!(resp.assistant.blocks.len(), 1);
        match &resp.assistant.blocks[0] {
            Block::Text { text, cache_marker } => {
                assert_eq!(text, "hello world");
                assert!(cache_marker.is_none());
            }
            other => panic!("expected Text, got {other:?}"),
        }
        assert_eq!(resp.usage.input_tokens, 5);
        assert_eq!(resp.usage.output_tokens, 2);
    }

    #[test]
    fn plain_prose_is_not_misrecovered_as_tool_call() {
        // A quirky backend returning ordinary prose (no tool-call shape) must
        // keep its text block — `parse_raw_toolcall_text` returns None.
        let wire = wire_from(serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": "just some prose, no tool call" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
        }));
        let resp = decode_response(wire, Backend::Other);
        assert_eq!(resp.assistant.blocks.len(), 1);
        assert!(matches!(resp.assistant.blocks[0], Block::Text { .. }));
    }
}
