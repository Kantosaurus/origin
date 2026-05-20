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
const DEFAULT_MAX_TOKENS: u32 = 4096;

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
        Self {
            auth: AuthKind::OAuthBearer(token.into()),
            base: DEFAULT_BASE.to_string(),
            client: reqwest::Client::new(),
            cas: None,
            plan: None,
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
            AuthKind::OAuthBearer(token) => {
                builder.header("Authorization", format!("Bearer {token}"))
            }
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
        };

        let url = format!("{}/v1/messages", self.base);
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
                Err(ProviderError::RateLimit {
                    retry_after_secs: retry,
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

        let url = format!("{}/v1/messages", self.base);
        let resp = self
            .apply_auth(self.client.post(&url))
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&serde_json::json!({
                "model": req.model,
                "max_tokens": DEFAULT_MAX_TOKENS,
                "system": if req.system.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String(req.system.clone())
                },
                "messages": wire_messages,
                "tools": wire_tools,
                "stream": true,
            }))
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api(format!("status {status}: {text}")));
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
    let content = m
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(block_idx, b)| {
            // Emit a cache marker on a block when:
            // - a plan is present
            // - this is the first message (msg_idx == 0)
            // - block_idx is in the plan's marker_indices
            // TODO(N4.3/Phase 11): lift the `msg_idx == 0` gate once handle-substitution lands and
            // the planner's marker_indices() map to a stable section-to-wire-block index.
            // Also: enumerate over wire-block index (post `block_to_wire` filter) to avoid
            // index skew when assistant messages contain `Block::Thinking` (which is
            // dropped from the wire). At P3.2 only user messages (msg_idx == 0) carry
            // markers, so the skew is unreachable.
            let cache_control = if plan.is_some() && msg_idx == 0 && marker_indices.contains(&block_idx) {
                Some(wire::WireCacheControl::ephemeral())
            } else {
                None
            };
            block_to_wire(b, cache_control)
        })
        .collect();
    wire::WireMessage { role, content }
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

                // Phase 3 stub: any handle in the active turn is treated as Volatile.
                // The full section-to-block mapping arrives with the N4.3 encoder codegen in Phase 11.
                let band = plan.map_or(origin_planner::Band::Volatile, |_p| {
                    origin_planner::Band::Volatile
                });

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

fn short_hex(h: &[u8; 32]) -> String {
    origin_cas::Hash::from_bytes(*h)
        .to_string()
        .chars()
        .take(8)
        .collect()
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
