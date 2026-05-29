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
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use reqwest::StatusCode;
use serde_json::json;

const DEFAULT_BASE: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 16_384;

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
}

#[async_trait]
impl Provider for Anthropic {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
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
            max_tokens: DEFAULT_MAX_TOKENS,
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
        };

        let url = self.messages_url();
        let resp = self
            .apply_auth(self.client.post(&url))
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        match resp.status() {
            StatusCode::OK => {
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
            "max_tokens": DEFAULT_MAX_TOKENS,
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

        let url = self.messages_url();
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

        let byte_stream = resp.bytes_stream();
        let async_read = tokio_util::io::StreamReader::new(
            byte_stream.map(|r| r.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))),
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
        max_tokens: DEFAULT_MAX_TOKENS,
        system: if req.system.is_empty() {
            None
        } else {
            Some(req.system.as_str())
        },
        messages: wire_messages,
        tools: wire_tools,
        metadata: None,
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
