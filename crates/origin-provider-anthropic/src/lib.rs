// SPDX-License-Identifier: Apache-2.0
//! Anthropic Messages API provider (non-streaming, API key auth).
//!
//! Streaming support arrives in Phase 2 (origin spec N4.4).

pub mod streaming;
mod wire;

use async_trait::async_trait;
use futures_util::StreamExt;
use origin_core::types::{Block, Message, Role};
use origin_planner::Plan;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, ReasoningEffort, Usage};
use reqwest::StatusCode;
use serde_json::json;

const DEFAULT_BASE: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 16_384;

/// Resolve the top-level `max_tokens` for a turn given an optional extended-
/// thinking budget.
///
/// Anthropic requires `max_tokens` to be strictly greater than the thinking
/// `budget_tokens`. When `thinking_tokens` is `None` we keep [`DEFAULT_MAX_TOKENS`]
/// verbatim, so the unset path is byte-identical to the pre-thinking behavior.
/// When set, we reserve [`DEFAULT_MAX_TOKENS`] of visible-output headroom on top
/// of the budget (`budget + DEFAULT_MAX_TOKENS`), which both satisfies the
/// `max_tokens > budget` constraint and leaves room for the model's actual
/// answer after it finishes thinking. Saturating add keeps it total on overflow.
const fn resolve_max_tokens(thinking_tokens: Option<u32>) -> u32 {
    match thinking_tokens {
        None => DEFAULT_MAX_TOKENS,
        Some(budget) => budget.saturating_add(DEFAULT_MAX_TOKENS),
    }
}

const OAUTH_BETA_HEADERS: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,advisor-tool-2026-03-01,advanced-tool-use-2025-11-20,effort-2025-11-24";
const CLAUDE_CLI_USER_AGENT: &str = "claude-cli/2.1.123 (external, sdk-cli)";
const OAUTH_BILLING_HEADER: &str = "cc_version=2.1.123; cc_entrypoint=sdk-cli; cch=33f85;";

/// Controls which auth header is sent with each request.
enum AuthKind {
    /// `x-api-key: <key>` — the standard Anthropic API key path.
    ApiKey(String),
    /// `Authorization: Bearer <token>` — used when a refreshed OAuth token is
    /// presented instead of a static API key.
    OAuthBearer(String),
}

/// Anthropic provider backed by the Messages API with API key authentication.
pub struct Anthropic {
    auth: AuthKind,
    base: String,
    client: reqwest::Client,
    cas: Option<std::sync::Arc<origin_cas::Store>>,
    plan: Option<Plan>,
    oauth_session_id: String,
    oauth_metadata: Option<wire::WireMetadata>,
}

impl Anthropic {
    /// Construct with the default base URL.
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE)
    }

    /// Construct against an arbitrary base URL (for testing).
    #[must_use]
    pub fn with_base_url(api_key: impl Into<String>, base: &str) -> Self {
        Self {
            auth: AuthKind::ApiKey(api_key.into()),
            base: base.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
            cas: None,
            plan: None,
            oauth_session_id: String::new(),
            oauth_metadata: None,
        }
    }

    /// Construct against an arbitrary endpoint URL with an API key and default
    /// model (for testing).
    ///
    /// The `model` parameter is accepted for symmetry with other test helpers
    /// but is not stored — the model is taken from `ChatRequest` at call time.
    #[must_use]
    pub fn with_endpoint(base: impl AsRef<str>, api_key: impl Into<String>, _model: &str) -> Self {
        Self::with_base_url(api_key, base.as_ref())
    }

    /// Construct using a refreshed OAuth bearer token instead of an API key.
    ///
    /// Sends `Authorization: Bearer <token>` rather than `x-api-key: <key>`.
    #[must_use]
    pub fn with_oauth_bearer(token: impl Into<String>) -> Self {
        let session_id = uuid::Uuid::new_v4().to_string();
        let metadata = load_oauth_metadata(&session_id);
        Self {
            auth: AuthKind::OAuthBearer(token.into()),
            base: DEFAULT_BASE.to_string(),
            client: reqwest::Client::new(),
            cas: None,
            plan: None,
            oauth_session_id: session_id,
            oauth_metadata: Some(metadata),
        }
    }

    /// Override the base URL (builder-pattern; useful for testing or proxies).
    #[must_use]
    pub fn with_base(mut self, base: &str) -> Self {
        self.base = base.trim_end_matches('/').to_string();
        self
    }

    /// Attach a CAS so `ToolResult` blocks carrying a handle are re-inflated
    /// from CAS bytes when serializing to the wire.
    #[must_use]
    pub fn with_cas(mut self, cas: std::sync::Arc<origin_cas::Store>) -> Self {
        self.cas = Some(cas);
        self
    }

    /// Attach a `Plan` so the encoder emits `cache_control` markers at the
    /// planned band boundaries.
    #[must_use]
    pub fn with_plan(mut self, plan: Plan) -> Self {
        self.plan = Some(plan);
        self
    }

    /// Apply the appropriate auth header to a request builder.
    fn apply_auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            AuthKind::ApiKey(key) => builder.header("x-api-key", key),
            AuthKind::OAuthBearer(token) => builder
                .header("Authorization", format!("Bearer {token}"))
                .header("User-Agent", CLAUDE_CLI_USER_AGENT)
                .header("anthropic-beta", OAUTH_BETA_HEADERS)
                .header("x-app", "cli")
                .header("X-Claude-Code-Session-Id", &self.oauth_session_id)
                .header("x-client-request-id", uuid::Uuid::new_v4().to_string())
                .header("X-Stainless-Arch", std::env::consts::ARCH)
                .header("X-Stainless-Lang", "js")
                .header("X-Stainless-OS", std::env::consts::OS)
                .header("X-Stainless-Package-Version", "0.81.0")
                .header("X-Stainless-Retry-Count", "0")
                .header("X-Stainless-Runtime", "node")
                .header("X-Stainless-Runtime-Version", "v24.3.0")
                .header("X-Stainless-Timeout", "600")
                .header("anthropic-dangerous-direct-browser-access", "true"),
        }
    }

    const fn is_oauth(&self) -> bool {
        matches!(self.auth, AuthKind::OAuthBearer(_))
    }

    fn messages_url(&self) -> String {
        if self.is_oauth() {
            format!("{}/v1/messages?beta=true", self.base)
        } else {
            format!("{}/v1/messages", self.base)
        }
    }

    /// Build the JSON request body for `chat`: typed wire encode (messages,
    /// tools, system, effort, oauth metadata) plus multimodal attachment
    /// injection. Returns the body as a `serde_json::Value` ready to POST.
    ///
    /// With no attachments the result is byte-identical to serializing the typed
    /// [`wire::WireRequest`]; with attachments, [`append_attachments`] adds the
    /// encoded image/text blocks to the last user message's content array.
    ///
    /// # Errors
    /// Propagates [`ProviderError`] from CAS handle expansion, or
    /// [`ProviderError::Api`] if the typed body fails to serialize.
    fn build_chat_body(&self, req: &ChatRequest) -> Result<serde_json::Value, ProviderError> {
        let expanded = expand_messages_for_wire(&req.messages, self.cas.as_ref(), self.plan.as_ref())?;
        let plan = self.plan.as_ref();
        let wire_messages = expanded
            .iter()
            .enumerate()
            .map(|(idx, m)| message_to_wire(m, plan, idx))
            .collect::<Vec<_>>();
        let wire_tools = req
            .tools
            .iter()
            .map(|t| wire::WireTool {
                name: &t.name,
                description: &t.description,
                input_schema: serde_json::from_str(&t.input_schema_json).unwrap_or_else(|_| json!({})),
            })
            .collect::<Vec<_>>();

        let system_text = if self.is_oauth() && !req.system.is_empty() {
            format!(
                "x-anthropic-billing-header: {}\n\n{}",
                OAUTH_BILLING_HEADER, req.system
            )
        } else {
            req.system.clone()
        };

        let body = wire::WireRequest {
            model: &req.model,
            // Bump `max_tokens` above the thinking budget when one is set
            // (Anthropic requires `max_tokens` > `budget_tokens`); otherwise
            // unchanged at `DEFAULT_MAX_TOKENS` ⇒ byte-identical default.
            max_tokens: resolve_max_tokens(req.thinking_tokens),
            system: if system_text.is_empty() {
                None
            } else {
                Some(system_text.as_str())
            },
            messages: wire_messages,
            tools: wire_tools,
            metadata: self.oauth_metadata.as_ref().map(|m| wire::WireMetadata {
                user_id: m.user_id.clone(),
            }),
            effort: req.effort.map(ReasoningEffort::as_wire_str),
            thinking: req.thinking_tokens.map(wire::WireThinking::enabled),
        };

        let mut body_value =
            serde_json::to_value(&body).map_err(|e| ProviderError::Api(format!("encode: {e}")))?;
        append_attachments(&mut body_value, &req.attachments);
        Ok(body_value)
    }
}

#[async_trait]
impl Provider for Anthropic {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        // Build the JSON request body (typed wire encode + multimodal attachment
        // injection). Extracted so `chat` stays under the line limit.
        let body_value = self.build_chat_body(&req)?;

        // Optional cassette tap (env `ORIGIN_CASSETTE=record:<path>|replay:<path>`).
        // Default (unset) returns `None`, so the network path below is unchanged
        // and byte-identical to the pre-cassette behavior.
        let cassette_mode = cassette::Mode::from_env();
        let url = self.messages_url();
        let req_body_text = serde_json::to_string(&body_value).unwrap_or_default();

        // Replay mode: serve the recorded response from disk with no network I/O.
        if let Some(cassette::Mode::Replay(path)) = &cassette_mode {
            return cassette::replay(path, "POST", &url, &req_body_text).and_then(|text| {
                serde_json::from_str::<wire::WireResponse>(&text)
                    .map(decode_response)
                    .map_err(|e| ProviderError::Api(format!("cassette decode: {e}")))
            });
        }

        let resp = self
            .apply_auth(self.client.post(&url))
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&body_value)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        match resp.status() {
            StatusCode::OK => {
                // When recording, capture the raw body text so it can be replayed
                // later (after secret scrubbing); otherwise decode directly.
                if let Some(cassette::Mode::Record(path)) = &cassette_mode {
                    let text = resp
                        .text()
                        .await
                        .map_err(|e| ProviderError::Api(format!("decode: {e}")))?;
                    cassette::record(path, "POST", &url, &req_body_text, 200, &text)?;
                    let wire: wire::WireResponse = serde_json::from_str(&text)
                        .map_err(|e| ProviderError::Api(format!("decode: {e}")))?;
                    return Ok(decode_response(wire));
                }
                let wire: wire::WireResponse = resp
                    .json()
                    .await
                    .map_err(|e| ProviderError::Api(format!("decode: {e}")))?;
                Ok(decode_response(wire))
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(ProviderError::Auth),
            StatusCode::TOO_MANY_REQUESTS => {
                let retry = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(1);
                let body = resp.text().await.unwrap_or_default();
                let message = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("error")?.get("message")?.as_str().map(String::from))
                    .unwrap_or_default();
                Err(ProviderError::RateLimit {
                    retry_after_secs: retry,
                    message,
                })
            }
            s => {
                let body = resp.text().await.unwrap_or_default();
                Err(ProviderError::Api(format!("status {s}: {body}")))
            }
        }
    }

    #[allow(clippy::too_many_lines)] // cohesive streaming method: body build + status handling + optional cassette record/replay tap
    async fn chat_stream(&self, req: ChatRequest, ring: &origin_stream::Ring) -> Result<(), ProviderError> {
        let expanded = expand_messages_for_wire(&req.messages, self.cas.as_ref(), self.plan.as_ref())?;
        let plan = self.plan.as_ref();
        let wire_messages = expanded
            .iter()
            .enumerate()
            .map(|(idx, m)| message_to_wire(m, plan, idx))
            .collect::<Vec<_>>();
        let wire_tools = req
            .tools
            .iter()
            .map(|t| wire::WireTool {
                name: &t.name,
                description: &t.description,
                input_schema: serde_json::from_str(&t.input_schema_json).unwrap_or_else(|_| json!({})),
            })
            .collect::<Vec<_>>();

        let system_text = if self.is_oauth() && !req.system.is_empty() {
            format!(
                "x-anthropic-billing-header: {}\n\n{}",
                OAUTH_BILLING_HEADER, req.system
            )
        } else {
            req.system.clone()
        };

        let mut body_json = serde_json::json!({
            "model": req.model,
            // Bump `max_tokens` above the thinking budget when one is set
            // (Anthropic requires `max_tokens` > `budget_tokens`); otherwise
            // unchanged at `DEFAULT_MAX_TOKENS` ⇒ byte-identical default.
            "max_tokens": resolve_max_tokens(req.thinking_tokens),
            "system": if system_text.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(system_text)
            },
            "messages": wire_messages,
            "tools": wire_tools,
            "stream": true,
        });
        if let Some(meta) = &self.oauth_metadata {
            body_json["metadata"] = serde_json::json!({ "user_id": meta.user_id });
        }
        // Emit the effort hint only when set, so the unset path is byte-identical.
        if let Some(level) = req.effort {
            body_json["effort"] = serde_json::Value::String(level.as_wire_str().to_string());
        }
        // Emit the extended-thinking block only when a budget is set, so the
        // unset path is byte-identical. `max_tokens` was already bumped above to
        // exceed the budget, per Anthropic's requirement.
        if let Some(budget) = req.thinking_tokens {
            body_json["thinking"] = serde_json::json!({
                "type": "enabled",
                "budget_tokens": budget,
            });
        }
        // Inject multimodal attachments into the last user message, mirroring the
        // non-streaming `chat` path (which injects via `build_chat_body`).
        // Streaming is the DEFAULT path, so without this every streamed turn
        // silently dropped attached images/PDFs. No-op when empty ⇒
        // byte-identical to the pre-attachment wire.
        append_attachments(&mut body_json, &req.attachments);

        let url = self.messages_url();

        // Optional cassette tap on the STREAMING path (env
        // `ORIGIN_CASSETTE=record:<path>|replay:<path>`). Default (unset) returns
        // `None`, so the network path below is unchanged and byte-identical to
        // the pre-cassette behavior. The recorded body is the raw SSE event-stream
        // text, replayed through `parse_into_ring` exactly as a live response.
        let cassette_mode = cassette::Mode::from_env();
        let req_body_text = serde_json::to_string(&body_json).unwrap_or_default();

        // Replay mode: serve the recorded SSE text from disk with no network I/O.
        // `&[u8]` is an `AsyncRead`, so the buffered text flows through the same
        // SSE → ring parser a live byte stream would.
        if let Some(cassette::Mode::Replay(path)) = &cassette_mode {
            let sse = cassette::replay(path, "POST", &url, &req_body_text)?;
            crate::streaming::parse_into_ring(sse.as_bytes(), ring)
                .await
                .map_err(|e| ProviderError::Api(e.to_string()))?;
            ring.close();
            return Ok(());
        }

        let resp = self
            .apply_auth(self.client.post(&url))
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&body_json)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        match resp.status() {
            StatusCode::OK => {}
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(ProviderError::Auth),
            StatusCode::TOO_MANY_REQUESTS => {
                let retry = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(1);
                let body = resp.text().await.unwrap_or_default();
                let message = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("error")?.get("message")?.as_str().map(String::from))
                    .unwrap_or_default();
                return Err(ProviderError::RateLimit {
                    retry_after_secs: retry,
                    message,
                });
            }
            s => {
                let text = resp.text().await.unwrap_or_default();
                return Err(ProviderError::Api(format!("status {s}: {text}")));
            }
        }

        // Record mode: buffer the whole SSE body to text, persist it (after
        // secret scrubbing + the save gate), THEN replay the buffered text into
        // the ring so the live caller still streams. Buffering is acceptable here
        // because recording is a test/dev affordance, not the hot path.
        if let Some(cassette::Mode::Record(path)) = &cassette_mode {
            let sse = resp
                .text()
                .await
                .map_err(|e| ProviderError::Api(format!("stream decode: {e}")))?;
            cassette::record(path, "POST", &url, &req_body_text, 200, &sse)?;
            crate::streaming::parse_into_ring(sse.as_bytes(), ring)
                .await
                .map_err(|e| ProviderError::Api(e.to_string()))?;
            ring.close();
            return Ok(());
        }

        let byte_stream = resp.bytes_stream();
        let async_read = tokio_util::io::StreamReader::new(
            byte_stream.map(|r| r.map_err(std::io::Error::other)),
        );
        crate::streaming::parse_into_ring(async_read, ring)
            .await
            .map_err(|e| ProviderError::Api(e.to_string()))?;
        ring.close();
        Ok(())
    }
}

fn message_to_wire<'a>(m: &'a Message, plan: Option<&Plan>, msg_idx: usize) -> wire::WireMessage<'a> {
    let role = match m.role {
        Role::User | Role::Tool | Role::System => "user",
        // Anthropic represents tool results as user messages (Role::Tool).
        // System content goes in the top-level `system` field, not a message (Role::System).
        Role::Assistant => "assistant",
    };
    let marker_indices: &[usize] = plan.map_or(&[], Plan::marker_indices);
    // Dynamic per-message markers (populated each turn by the agent loop).
    // Empty by default, so the read is cheap when no planner is wired.
    let dyn_msg_marker_here = plan.is_some_and(|p| p.dynamic_message_markers().contains(&msg_idx));
    // When path 3 fires we need to land the marker on the *last emitting*
    // block. `Block::Thinking` is filtered out by `block_to_wire`, so we skip
    // it when picking the boundary block — otherwise a trailing thinking
    // block would silently swallow the cache marker.
    let last_emit_idx = if dyn_msg_marker_here {
        m.blocks
            .iter()
            .rposition(|b| !matches!(b, Block::Thinking { .. }))
    } else {
        None
    };
    // Count emitted `cache_control` markers across the message so we can warn
    // when callers approach Anthropic's per-request 4-marker ceiling. The
    // warn fires at the message level; aggregating across messages would
    // require a wider pass, so we settle for per-message visibility here.
    let mut emitted_markers: usize = 0;
    let content = m
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(block_idx, b)| {
            // Three paths can emit a `cache_control` on a block:
            //
            // 1. A `Plan` planted a marker at `(msg_idx == 0, block_idx)`
            //    via `Plan::marker_indices`. This is the legacy P3.2 path
            //    for first-message section boundaries.
            //
            // 2. The block itself carries `cache_marker: Some(_)`. Phase 11
            //    handle-substitution makes section markers viable on any
            //    message (not just msg 0), so we honour the in-band marker
            //    regardless of `msg_idx`. All `CacheBoundary` variants map
            //    to `"ephemeral"`: that is the only `cache_control.type`
            //    the Anthropic Messages API accepts today.
            //
            // 3. The agent loop populated `dynamic_message_markers` with
            //    `msg_idx`. The marker lands on the *last emitting* block
            //    of this message — i.e., the natural turn boundary. Skipped
            //    when the planner is absent or the index list is empty.
            let plan_marker_here = plan.is_some() && msg_idx == 0 && marker_indices.contains(&block_idx);
            let block_marker_here = block_has_cache_marker(b);
            let dynamic_marker_here = Some(block_idx) == last_emit_idx;
            let cache_control = if plan_marker_here || block_marker_here || dynamic_marker_here {
                emitted_markers = emitted_markers.saturating_add(1);
                Some(wire::WireCacheControl::ephemeral())
            } else {
                None
            };
            block_to_wire(b, cache_control)
        })
        .collect();
    if emitted_markers > 4 {
        tracing::warn!(
            msg_idx,
            emitted_markers,
            "Anthropic accepts at most 4 cache_control markers per request; \
             the API will reject the overflow. Trim cache markers or split the request."
        );
    }
    wire::WireMessage { role, content }
}

const fn block_has_cache_marker(b: &Block) -> bool {
    match b {
        Block::Text { cache_marker, .. }
        | Block::ToolUse { cache_marker, .. }
        | Block::ToolResult { cache_marker, .. } => cache_marker.is_some(),
        Block::Thinking { .. } => false,
    }
}

fn block_to_wire(b: &Block, cache_control: Option<wire::WireCacheControl>) -> Option<wire::WireBlock<'_>> {
    match b {
        Block::Text { text, .. } => Some(wire::WireBlock::Text { text, cache_control }),
        Block::ToolUse {
            id, name, input_json, ..
        } => {
            let input: serde_json::Value = serde_json::from_slice(input_json).unwrap_or_else(|_| json!({}));
            Some(wire::WireBlock::ToolUse {
                id,
                name,
                input,
                cache_control,
            })
        }
        Block::ToolResult {
            tool_use_id, inline, ..
        } => {
            // Borrow the inline bytes; convert to &str via str::from_utf8 with a fallback.
            let content_str: &str = inline
                .as_deref()
                .and_then(|v| std::str::from_utf8(v).ok())
                .unwrap_or("");
            Some(wire::WireBlock::ToolResult {
                tool_use_id,
                content: content_str,
                is_error: false,
                cache_control,
            })
        }
        // Do not re-send thinking blocks; Anthropic ignores them on inbound.
        Block::Thinking { .. } => None,
    }
}

/// Re-inflate any `ToolResult` blocks that carry a CAS `handle` (but no
/// inline bytes) by fetching the payload from the attached store.
///
/// When a `plan` is provided, consults `WireDecision` to decide whether to
/// inline the bytes or emit a short `<result handle:… — N bytes>` reference.
///
/// Blocks with inline bytes (or unrelated kinds) are passed through unchanged.
///
/// # Errors
/// Returns `ProviderError::Api` if a handle is encountered without a CAS, or
/// if the CAS lookup fails or misses.
fn expand_messages_for_wire(
    messages: &[Message],
    cas: Option<&std::sync::Arc<origin_cas::Store>>,
    plan: Option<&origin_planner::Plan>,
) -> Result<Vec<Message>, ProviderError> {
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        let mut blocks = Vec::with_capacity(m.blocks.len());
        for b in &m.blocks {
            if let Block::ToolResult {
                tool_use_id,
                handle: Some(h),
                inline: None,
                cache_marker,
            } = b
            {
                let store = cas.ok_or_else(|| {
                    ProviderError::Api("ToolResult handle present but no CAS configured".into())
                })?;
                let bytes = store
                    .get(origin_cas::Hash::from_bytes(*h))
                    .map_err(|e| ProviderError::Api(format!("cas get: {e}")))?
                    .ok_or_else(|| ProviderError::Api("cas miss for tool result handle".into()))?;

                // N4.3: consult the per-handle band index in `O(1)`. The
                // planner populates this via `Plan::register_handle` as
                // it threads sections through the compactor; callers that
                // have not registered a particular handle fall through to
                // the safe floor (`Band::Volatile`), which preserves the
                // pre-N4.3 behavior of aggressive inlining.
                //
                // This is the novel angle that beats openclaude/jcode/
                // opencode on tokens: they re-serialize every tool result
                // unconditionally, while we can demote long-lived handles
                // to a short `<result handle:… — N bytes>` reference.
                let band = plan
                    .and_then(|p| p.band_for_handle(h))
                    .unwrap_or(origin_planner::Band::Volatile);

                match origin_planner::WireDecision::for_block(band, bytes.len()) {
                    origin_planner::WireDecision::Inline => {
                        blocks.push(Block::ToolResult {
                            tool_use_id: tool_use_id.clone(),
                            handle: None,
                            inline: Some(bytes),
                            cache_marker: *cache_marker,
                        });
                    }
                    origin_planner::WireDecision::Reference => {
                        let preview =
                            format!("<result handle:{} \u{2014} {} bytes>", short_hex(h), bytes.len());
                        blocks.push(Block::ToolResult {
                            tool_use_id: tool_use_id.clone(),
                            handle: None,
                            inline: Some(preview.into_bytes()),
                            cache_marker: *cache_marker,
                        });
                    }
                }
            } else {
                blocks.push(b.clone());
            }
        }
        out.push(Message { role: m.role, blocks });
    }
    Ok(out)
}

fn load_oauth_metadata(session_id: &str) -> wire::WireMetadata {
    let home = dirs::home_dir().unwrap_or_default();
    let claude_json = home.join(".claude.json");
    let (device_id, account_uuid) = std::fs::read_to_string(&claude_json)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .map_or_else(
            || {
                let did = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, session_id.as_bytes())
                    .simple()
                    .to_string();
                (did, "unknown-account".to_string())
            },
            |v| {
                let did = v.get("userID").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let auid = v
                    .get("oauthAccount")
                    .and_then(|o| o.get("accountUuid"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("unknown-account")
                    .to_string();
                (did, auid)
            },
        );

    let user_id = serde_json::json!({
        "device_id": device_id,
        "account_uuid": account_uuid,
        "session_id": session_id,
    })
    .to_string();
    wire::WireMetadata { user_id }
}

/// Append multimodal attachments to the last user message's content array
/// (item G-live).
///
/// Each attachment is encoded to its Anthropic content-block JSON via
/// [`origin_multimodal::encode_anthropic_block`] and pushed onto the `content`
/// array of the last `{"role":"user"}` message. A no-op when `attachments` is
/// empty, so the default text-only request is byte-identical. If there is no
/// user message (an unusual but valid request), a new user message carrying the
/// attachments is appended so the blocks are never silently dropped.
fn append_attachments(body: &mut serde_json::Value, attachments: &[origin_multimodal::ContentBlock]) {
    if attachments.is_empty() {
        return;
    }
    let encoded: Vec<serde_json::Value> = attachments
        .iter()
        .map(origin_multimodal::encode_anthropic_block)
        .collect();
    let Some(messages) = body.get_mut("messages").and_then(serde_json::Value::as_array_mut) else {
        return;
    };
    let last_user = messages
        .iter_mut()
        .rev()
        .find(|m| m.get("role").and_then(serde_json::Value::as_str) == Some("user"));
    if let Some(msg) = last_user {
        if let Some(content) = msg.get_mut("content").and_then(serde_json::Value::as_array_mut) {
            content.extend(encoded);
            return;
        }
    }
    // No user message with an array content: append a fresh one.
    messages.push(serde_json::json!({ "role": "user", "content": encoded }));
}

/// Cassette tap on the non-streaming `chat()` path (item I).
///
/// Records every provider request/response into an `origin-cassette` file when
/// `ORIGIN_CASSETTE=record:<path>` is set, scrubbing secrets before persisting;
/// serves the recorded response without any network I/O when
/// `ORIGIN_CASSETTE=replay:<path>` is set. With the variable unset, nothing in
/// this module runs and the chat path is byte-identical to before.
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
        /// Parse the `ORIGIN_CASSETTE` env var. Returns `None` when unset or
        /// malformed, so the default (no-cassette) path is unaffected.
        #[must_use]
        pub fn from_env() -> Option<Self> {
            let raw = std::env::var("ORIGIN_CASSETTE").ok()?;
            if let Some(p) = raw.strip_prefix("record:") {
                return (!p.is_empty()).then(|| Self::Record(p.to_string()));
            }
            if let Some(p) = raw.strip_prefix("replay:") {
                return (!p.is_empty()).then(|| Self::Replay(p.to_string()));
            }
            tracing::warn!(
                value = %raw,
                "ORIGIN_CASSETTE must be `record:<path>` or `replay:<path>`; ignoring"
            );
            None
        }
    }

    /// Append a request/response interaction to the cassette at `path`,
    /// scrubbing secrets before persisting and refusing to save if any leak
    /// remains.
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
            Err(_) => Cassette::new("anthropic"),
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
        // Scrub credentials, then hard-gate the save so a live token can never
        // be persisted to a cassette file.
        origin_cassette::scrub_secrets(&mut cassette);
        origin_cassette::assert_redacted(&cassette)
            .map_err(|e| ProviderError::Api(format!("cassette redaction gate: {e}")))?;
        let json = cassette
            .to_json()
            .map_err(|e| ProviderError::Api(format!("cassette serialize: {e}")))?;
        std::fs::write(path, json).map_err(|e| ProviderError::Api(format!("cassette write: {e}")))
    }

    /// Replay the recorded response body for a `(method, url)` request from the
    /// cassette at `path`.
    ///
    /// # Errors
    /// Returns [`ProviderError::Api`] if the cassette cannot be read/parsed, no
    /// matching interaction exists, or the recorded status is non-OK.
    pub fn replay(
        path: &str,
        method: &str,
        url: &str,
        req_body: &str,
    ) -> Result<String, ProviderError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ProviderError::Api(format!("cassette read: {e}")))?;
        let cassette = Cassette::from_json(&text)
            .map_err(|e| ProviderError::Api(format!("cassette parse: {e}")))?;
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

fn short_hex(h: &[u8; 32]) -> String {
    origin_cas::Hash::from_bytes(*h)
        .to_string()
        .chars()
        .take(8)
        .collect()
}

/// Test-only: build the JSON body that `Provider::chat` would POST to
/// `/v1/messages`, without any plan, CAS expansion, or network I/O.
///
/// This mirrors the encode pipeline used in `Provider::chat` but returns the
/// `serde_json::Value` body directly so integration tests can assert on the
/// wire shape (in particular, the placement of `cache_control` markers).
///
/// Marked `#[doc(hidden)]` so it does not bloat the public docs surface — it
/// is not part of the supported API and may change without a semver bump.
///
/// # Panics
/// Panics if serialisation fails. Serialisation here cannot fail in practice
/// (all fields are owned `String` / `Vec<u8>` / `&str`); callers may
/// `expect("encode")` on the surrounding flow as needed.
#[doc(hidden)]
#[must_use]
pub fn encode_request_for_test(req: &ChatRequest) -> serde_json::Value {
    let wire_messages = req
        .messages
        .iter()
        .enumerate()
        .map(|(idx, m)| message_to_wire(m, None, idx))
        .collect::<Vec<_>>();
    let wire_tools = req
        .tools
        .iter()
        .map(|t| wire::WireTool {
            name: &t.name,
            description: &t.description,
            input_schema: serde_json::from_str(&t.input_schema_json).unwrap_or_else(|_| json!({})),
        })
        .collect::<Vec<_>>();
    let body = wire::WireRequest {
        model: &req.model,
        max_tokens: resolve_max_tokens(req.thinking_tokens),
        system: if req.system.is_empty() {
            None
        } else {
            Some(req.system.as_str())
        },
        messages: wire_messages,
        tools: wire_tools,
        metadata: None,
        effort: req.effort.map(ReasoningEffort::as_wire_str),
        thinking: req.thinking_tokens.map(wire::WireThinking::enabled),
    };
    serde_json::to_value(&body).expect("WireRequest serialises to JSON")
}

fn decode_response(wire: wire::WireResponse) -> ChatResponse {
    let blocks = wire
        .content
        .into_iter()
        .map(|b| match b {
            wire::WireRespBlock::Text { text } => Block::Text {
                text,
                cache_marker: None,
            },
            wire::WireRespBlock::ToolUse { id, name, input } => Block::ToolUse {
                id,
                name,
                input_json: serde_json::to_vec(&input).unwrap_or_default(),
                cache_marker: None,
            },
            wire::WireRespBlock::Thinking { thinking, signature } => Block::Thinking {
                tokens: thinking,
                signature,
            },
        })
        .collect();

    let assistant = Message {
        role: Role::Assistant,
        blocks,
    };
    let usage = Usage {
        input_tokens: wire.usage.input_tokens,
        output_tokens: wire.usage.output_tokens,
        cache_read_input_tokens: wire.usage.cache_read_input_tokens,
        cache_creation_input_tokens: wire.usage.cache_creation_input_tokens,
    };
    ChatResponse { assistant, usage }
}
