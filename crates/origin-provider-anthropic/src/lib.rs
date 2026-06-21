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

/// Whether `model` uses *adaptive* thinking (`{"type":"adaptive"}`) rather than
/// the legacy fixed-budget form. The Claude 4.6/4.7/4.8 families
/// (e.g. `claude-opus-4-7`, `claude-opus-4-8`, `claude-sonnet-4-6`) require/
/// recommend adaptive thinking — manual `budget_tokens` is deprecated on 4.6 and
/// **returns a 400 on 4.7+**. Claude 4.5 and earlier use the legacy form.
fn model_uses_adaptive_thinking(model: &str) -> bool {
    ["-4-6", "-4-7", "-4-8"].iter().any(|tag| model.contains(tag))
}

/// Build the `thinking` block for `model` from a requested budget, choosing
/// adaptive vs. legacy fixed-budget per [`model_uses_adaptive_thinking`].
fn thinking_block(model: &str, budget_tokens: u32) -> wire::WireThinking {
    if model_uses_adaptive_thinking(model) {
        wire::WireThinking::Adaptive
    } else {
        wire::WireThinking::Enabled { budget_tokens }
    }
}

/// Build the `output_config` block carrying the reasoning effort (Messages API
/// `output_config.effort`, not a top-level `effort` field).
const fn output_config(effort: ReasoningEffort) -> wire::WireOutputConfig {
    wire::WireOutputConfig {
        effort: Some(effort.as_anthropic_effort()),
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
        let wire_messages = build_wire_messages(&expanded, plan);
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
            output_config: req.effort.map(output_config),
            thinking: req.thinking_tokens.map(|b| thinking_block(&req.model, b)),
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
            s if status_is_transient(s) => Err(transient_rate_limit(resp).await),
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
        let wire_messages = build_wire_messages(&expanded, plan);
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
        // Emit the effort hint only when set (nested under output_config, per the
        // Messages API), so the unset path is byte-identical.
        if let Some(level) = req.effort {
            body_json["output_config"] = serde_json::json!({ "effort": level.as_anthropic_effort() });
        }
        // Emit the extended-thinking block only when a budget is set, so the
        // unset path is byte-identical. Adaptive for 4.6/4.7/4.8 (manual
        // budget_tokens 400s on 4.7+); legacy enabled+budget for older models —
        // `max_tokens` was already bumped above to exceed the budget for those.
        if let Some(budget) = req.thinking_tokens {
            body_json["thinking"] = if model_uses_adaptive_thinking(&req.model) {
                serde_json::json!({ "type": "adaptive" })
            } else {
                serde_json::json!({ "type": "enabled", "budget_tokens": budget })
            };
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
            s if status_is_transient(s) => return Err(transient_rate_limit(resp).await),
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
        let async_read =
            tokio_util::io::StreamReader::new(byte_stream.map(|r| r.map_err(std::io::Error::other)));
        crate::streaming::parse_into_ring(async_read, ring)
            .await
            .map_err(|e| ProviderError::Api(e.to_string()))?;
        ring.close();
        Ok(())
    }
}

/// True for HTTP statuses worth retrying with backoff rather than killing the
/// turn. Mapped to [`ProviderError::RateLimit`] so the daemon's existing
/// retry/backoff loop handles them; everything else stays fatal
/// [`ProviderError::Api`].
fn status_is_transient(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        || status.as_u16() == 529 // Anthropic `overloaded_error`
        || status.is_server_error() // any 5xx
}

/// Build a [`ProviderError::RateLimit`] from a transient response: honor
/// `retry-after` (else 1s) and surface the API's error message.
async fn transient_rate_limit(resp: reqwest::Response) -> ProviderError {
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
    ProviderError::RateLimit {
        retry_after_secs: retry,
        message,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod transient_tests {
    use super::status_is_transient;
    use reqwest::StatusCode;

    #[test]
    fn retries_429_529_and_5xx_but_not_4xx() {
        assert!(status_is_transient(StatusCode::TOO_MANY_REQUESTS), "429 is retryable");
        assert!(
            status_is_transient(StatusCode::from_u16(529).unwrap()),
            "529 overloaded_error is retryable (the common turn-killer)"
        );
        assert!(status_is_transient(StatusCode::INTERNAL_SERVER_ERROR), "500 retryable");
        assert!(status_is_transient(StatusCode::BAD_GATEWAY), "502 retryable");
        assert!(status_is_transient(StatusCode::SERVICE_UNAVAILABLE), "503 retryable");
        assert!(!status_is_transient(StatusCode::BAD_REQUEST), "400 stays fatal");
        assert!(!status_is_transient(StatusCode::NOT_FOUND), "404 stays fatal");
    }
}

/// Anthropic's Messages API rejects any request carrying more than this many
/// blocks with `cache_control` (`invalid_request_error: "A maximum of 4 blocks
/// with cache_control may be provided."`). The ceiling spans the WHOLE request
/// (tools + system + messages); in this encoder `system` is a plain string and
/// `WireTool` carries no marker, so every marker lives on a message block and
/// this is the only place the budget is spent.
const WIRE_MAX_CACHE_MARKERS: usize = 4;

/// Block indices within message `m` (at `msg_idx`) that any emission path would
/// flag with `cache_control`, BEFORE the per-request ceiling is applied. Three
/// independent paths feed this:
///
/// 1. A `Plan` planted a marker at `(msg_idx == 0, block_idx)` via
///    `Plan::marker_indices` — the legacy P3.2 first-message section boundary.
/// 2. The block itself carries `cache_marker: Some(_)` — the index-immune,
///    block-anchored boundary the agent loop plants on turn boundaries. This is
///    the authoritative source: it rides along with its block through snapshot,
///    CAS expansion, and orphan-stripping, so it never drifts.
/// 3. The plan's `dynamic_message_markers` lists `msg_idx`; the marker lands on
///    the *last emitting* block (a trailing `Block::Thinking` is dropped by
///    `block_to_wire`, so it would otherwise swallow the marker). This path is
///    index-based and therefore fragile if the caller's index space diverges
///    from the wire's — `build_wire_messages` clamps the union regardless.
fn marker_block_indices(m: &Message, plan: Option<&Plan>, msg_idx: usize) -> Vec<usize> {
    let marker_indices: &[usize] = plan.map_or(&[], Plan::marker_indices);
    let dyn_msg_marker_here = plan.is_some_and(|p| p.dynamic_message_markers().contains(&msg_idx));
    let last_emit_idx = if dyn_msg_marker_here {
        m.blocks
            .iter()
            .rposition(|b| !matches!(b, Block::Thinking { .. }))
    } else {
        None
    };
    m.blocks
        .iter()
        .enumerate()
        .filter_map(|(block_idx, b)| {
            let plan_marker_here = plan.is_some() && msg_idx == 0 && marker_indices.contains(&block_idx);
            let block_marker_here = block_has_cache_marker(b);
            let dynamic_marker_here = Some(block_idx) == last_emit_idx;
            (plan_marker_here || block_marker_here || dynamic_marker_here).then_some(block_idx)
        })
        .collect()
}

/// Build the full `messages` array for the wire request, placing cache markers
/// and enforcing Anthropic's per-request ceiling across the WHOLE request.
///
/// This is the single choke point both `chat` (non-streaming) and `chat_stream`
/// funnel through, so the ceiling is enforced in exactly one place regardless of
/// transport — and regardless of how the upstream marker-selection paths overlap
/// or whether their index space has drifted from the wire's (e.g. after a turn
/// was dropped by `strip_orphan_tool_results`). Without this clamp such a drift
/// is fatal: the API 400s the entire turn with `"A maximum of 4 blocks with
/// cache_control may be provided. Found 5."`.
fn build_wire_messages<'a>(messages: &'a [Message], plan: Option<&Plan>) -> Vec<wire::WireMessage<'a>> {
    // Pass 1: collect every (msg_idx, block_idx) any path would mark, in
    // document order (ascending msg_idx, then block_idx).
    let selected: Vec<(usize, usize)> = messages
        .iter()
        .enumerate()
        .flat_map(|(msg_idx, m)| {
            marker_block_indices(m, plan, msg_idx)
                .into_iter()
                .map(move |block_idx| (msg_idx, block_idx))
        })
        .collect();

    // Clamp to the ceiling, keeping the LATEST markers: Anthropic's prompt cache
    // hits on prefix-extension, so a newer marker amortizes across more
    // subsequent turns than an older one. Since `selected` is in document order,
    // the last `WIRE_MAX_CACHE_MARKERS` entries are the latest.
    if selected.len() > WIRE_MAX_CACHE_MARKERS {
        tracing::warn!(
            selected = selected.len(),
            kept = WIRE_MAX_CACHE_MARKERS,
            "cache_control marker selection exceeded Anthropic's per-request ceiling; \
             keeping the latest and dropping the earliest to avoid a 400"
        );
    }
    let keep: std::collections::HashSet<(usize, usize)> = selected
        .iter()
        .rev()
        .take(WIRE_MAX_CACHE_MARKERS)
        .copied()
        .collect();

    // Pass 2: serialize, emitting `cache_control` only on the kept positions.
    messages
        .iter()
        .enumerate()
        .map(|(msg_idx, m)| message_to_wire(m, msg_idx, &keep))
        .collect()
}

fn message_to_wire<'a>(
    m: &'a Message,
    msg_idx: usize,
    keep: &std::collections::HashSet<(usize, usize)>,
) -> wire::WireMessage<'a> {
    let role = match m.role {
        Role::User | Role::Tool | Role::System => "user",
        // Anthropic represents tool results as user messages (Role::Tool).
        // System content goes in the top-level `system` field, not a message (Role::System).
        Role::Assistant => "assistant",
    };
    let content = m
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(block_idx, b)| {
            let cache_control = if keep.contains(&(msg_idx, block_idx)) {
                // Only the single most-stable boundary (tagged `Frozen` by the
                // agent loop) gets the longer 1h TTL, and only when explicitly
                // enabled — sending `ttl` requires the extended-cache-ttl beta,
                // which the default impersonation header set does not carry.
                let frozen = matches!(
                    block_cache_boundary(b),
                    Some(origin_core::types::CacheBoundary::Frozen)
                );
                Some(if frozen && ttl_1h_enabled() {
                    wire::WireCacheControl::ephemeral_1h()
                } else {
                    wire::WireCacheControl::ephemeral()
                })
            } else {
                None
            };
            block_to_wire(b, cache_control)
        })
        .collect();
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

/// The `CacheBoundary` tier carried by a block's cache marker, if any.
const fn block_cache_boundary(b: &Block) -> Option<origin_core::types::CacheBoundary> {
    match b {
        Block::Text { cache_marker, .. }
        | Block::ToolUse { cache_marker, .. }
        | Block::ToolResult { cache_marker, .. } => *cache_marker,
        Block::Thinking { .. } => None,
    }
}

/// Whether the opt-in 1-hour cache TTL is enabled (`ORIGIN_CACHE_TTL_1H=1`).
///
/// Off by default: emitting a `ttl` requires the Anthropic extended-cache-ttl
/// beta, and the default request carries an impersonation beta-header set that
/// must not be modified, so the longer TTL is opt-in until an account is known
/// to support it. Read once and cached.
fn ttl_1h_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("ORIGIN_CACHE_TTL_1H").as_deref() == Ok("1"))
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
/// Returns `ProviderError::Api` if a handle is encountered without a CAS, or if
/// the CAS lookup itself errors. A CAS *miss* degrades to an inline
/// `CAS_MISS_PLACEHOLDER` (payload lost in a restart) rather than failing.
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
                    .unwrap_or_else(|| {
                        // Degrade rather than fail the turn: the payload was lost
                        // in a daemon restart (Hot tier not flushed). Inline a
                        // placeholder so the tool_use/tool_result pairing holds.
                        tracing::warn!(
                            "cas miss for tool result handle; substituting placeholder \
                             (cached output lost across a daemon restart)"
                        );
                        origin_provider::CAS_MISS_PLACEHOLDER.as_bytes().to_vec()
                    });

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
    // Defense-in-depth at the API boundary: drop any `tool_result` whose
    // `tool_use` is missing from the preceding message. The Messages API rejects
    // such an orphan with a hard `400 ... unexpected tool_use_id found in
    // tool_result blocks`, taking down the whole turn over one malformed entry
    // deep in the history (e.g. a stranded tail spliced on after a reused session
    // id, or a compaction hole). Stripping it here keeps a corrupted transcript
    // recoverable instead of fatal; a well-formed transcript is unchanged.
    Ok(origin_core::types::strip_orphan_tool_results(out))
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
    pub fn replay(path: &str, method: &str, url: &str, _req_body: &str) -> Result<String, ProviderError> {
        // Durable, sequential replay: the position is persisted in a `<path>.pos`
        // sidecar so each turn of a multi-turn session consumes the NEXT recorded
        // interaction (turn 1, 2, 3, …) rather than replaying interaction[0] every
        // time. The cassette is re-read fresh each call, so the cursor must live
        // out-of-band on disk.
        let interaction = origin_cassette::replay_next(std::path::Path::new(path), method, url)
            .map_err(|e| ProviderError::Api(format!("cassette replay: {e}")))?;
        if interaction.response.status != 200 {
            return Err(ProviderError::Api(format!(
                "cassette replay status {}",
                interaction.response.status
            )));
        }
        Ok(interaction.response.body)
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
    encode_request_body_for_test(req, None)
}

/// Test-only sibling of [`encode_request_for_test`] that threads a [`Plan`] into
/// the wire encoder, so tests can exercise the plan-driven cache-marker paths
/// (planner `marker_indices` and `dynamic_message_markers`) and the global
/// per-request `cache_control` ceiling those paths feed into.
#[doc(hidden)]
#[must_use]
pub fn encode_request_with_plan_for_test(req: &ChatRequest, plan: &Plan) -> serde_json::Value {
    encode_request_body_for_test(req, Some(plan))
}

fn encode_request_body_for_test(req: &ChatRequest, plan: Option<&Plan>) -> serde_json::Value {
    // Mirror the production encode pipeline (expand CAS handles + strip orphan
    // tool_results) so tests observe the exact message list the wire encoder
    // places cache markers on — including any index shift from a dropped turn.
    let expanded = expand_messages_for_wire(&req.messages, None, plan)
        .expect("test requests carry no CAS handles, so expansion cannot fail");
    let wire_messages = build_wire_messages(&expanded, plan);
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
        output_config: req.effort.map(output_config),
        thinking: req.thinking_tokens.map(|b| thinking_block(&req.model, b)),
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
