// SPDX-License-Identifier: Apache-2.0
//! `GitHub Models` provider (non-streaming) — `OpenAI`-shape body with an
//! OAuth bearer token sourced from [`origin_keyvault::KeyVault`] on every
//! call.
//!
//! Implements the canonical `origin_provider::Provider` trait against
//! `POST {base}/inference/chat/completions`. The access token is read from
//! the vault under (`"github"`, `"{account}/oauth"`) on every `chat`
//! invocation so background refresh rotation (P8.2) is observed without
//! restarting the daemon.
#![allow(clippy::module_name_repetitions)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_keyvault::KeyVault;
use origin_provider::openai_tools::{tool_call_to_block, WireToolCall};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const DEFAULT_BASE: &str = "https://models.github.ai";

/// `GitHub Models` provider backed by `POST /inference/chat/completions`.
pub struct GitHubModels {
    vault: KeyVault,
    account: String,
    base: String,
    client: reqwest::Client,
}

impl GitHubModels {
    /// Construct with the default base URL (`https://models.github.ai`).
    #[must_use]
    pub fn new(vault: KeyVault, account: impl Into<String>) -> Self {
        Self::with_base_url(vault, account, DEFAULT_BASE)
    }

    /// Construct against an arbitrary base URL (for testing and gateways).
    #[must_use]
    pub fn with_base_url(vault: KeyVault, account: impl Into<String>, base: &str) -> Self {
        Self {
            vault,
            account: account.into(),
            base: base.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Fetch the current access token from the vault on every call so the
    /// background refresh rotation (P8.2) is observed without restart.
    async fn current_access_token(&self) -> Result<String, ProviderError> {
        let secret = self
            .vault
            .get("github", &format!("{}/oauth", self.account))
            .await
            .map_err(|_| ProviderError::Auth)?;
        let stored: StoredOAuthTokens = serde_json::from_str(secret.expose())
            .map_err(|e| ProviderError::Api(format!("stored token parse: {e}")))?;
        Ok(stored.access)
    }
}

#[async_trait]
impl Provider for GitHubModels {
    fn name(&self) -> &'static str {
        "github-models"
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let access = self.current_access_token().await?;
        let body = encode_request(&req);
        let url = format!("{}/inference/chat/completions", self.base);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&access)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        if resp.status() == StatusCode::OK {
            let wire: WireResponse = resp
                .json()
                .await
                .map_err(|e| ProviderError::Api(format!("decode: {e}")))?;
            return Ok(decode_response(wire));
        }
        Err(status_error(resp).await)
    }
}

/// Map a non-2xx `reqwest::Response` to the canonical `ProviderError` variant.
///
/// - 401 / 403 → `ProviderError::Auth`
/// - 429 → `ProviderError::RateLimit` (parses `retry-after` header, defaults to 1)
/// - other → `ProviderError::Api(format!("status {s}: {body}"))`
async fn status_error(resp: reqwest::Response) -> ProviderError {
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
            ProviderError::Api(format!("status {s}: {body}"))
        }
    }
}

fn decode_response(wire: WireResponse) -> ChatResponse {
    let mut blocks: Vec<Block> = Vec::new();
    if let Some(choice) = wire.choices.into_iter().next() {
        if let Some(text) = choice.message.content {
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

    let assistant = Message {
        role: Role::Assistant,
        blocks,
    };
    let usage = Usage {
        input_tokens: wire.usage.prompt_tokens,
        output_tokens: wire.usage.completion_tokens,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
    };
    ChatResponse { assistant, usage }
}

// ── wire types ───────────────────────────────────────────────────────────────

/// On-disk shape persisted by `origin-keyvault`'s OAuth driver. We only need
/// the `access` field; the rest is ignored so this code keeps working if the
/// schema gains additional fields.
#[derive(Deserialize)]
struct StoredOAuthTokens {
    access: String,
}

/// Top-level request body for `POST /inference/chat/completions`.
#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool<'a>>,
}

#[derive(Serialize)]
struct WireMessage {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<WireOutToolCall>>,
}

#[derive(Serialize)]
struct WireOutToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireOutToolFunction,
}

#[derive(Serialize)]
struct WireOutToolFunction {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct WireTool<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireToolFunctionSchema<'a>,
}

#[derive(Serialize)]
struct WireToolFunctionSchema<'a> {
    name: &'a str,
    description: &'a str,
    parameters: Value,
}

#[derive(Deserialize)]
struct WireResponse {
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: WireUsage,
}

#[derive(Deserialize)]
struct WireChoice {
    message: WireRespMessage,
    #[serde(default)]
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireRespMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireToolCall>>,
}

#[derive(Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

fn encode_request(req: &ChatRequest) -> WireRequest<'_> {
    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    if !req.system.is_empty() {
        messages.push(WireMessage {
            role: "system",
            content: Some(req.system.clone()),
            tool_call_id: None,
            tool_calls: None,
        });
    }
    for m in &req.messages {
        encode_message_into(m, &mut messages);
    }

    let tools = req
        .tools
        .iter()
        .map(|t| WireTool {
            kind: "function",
            function: WireToolFunctionSchema {
                name: &t.name,
                description: &t.description,
                parameters: serde_json::from_str(&t.input_schema_json).unwrap_or_else(|_| json!({})),
            },
        })
        .collect();

    WireRequest {
        model: &req.model,
        messages,
        tools,
    }
}

fn encode_message_into(m: &Message, out: &mut Vec<WireMessage>) {
    match m.role {
        Role::System => {
            let text = collect_text(&m.blocks);
            out.push(WireMessage {
                role: "system",
                content: Some(text),
                tool_call_id: None,
                tool_calls: None,
            });
        }
        Role::User => {
            let text = collect_text(&m.blocks);
            out.push(WireMessage {
                role: "user",
                content: Some(text),
                tool_call_id: None,
                tool_calls: None,
            });
        }
        Role::Assistant => {
            let text = collect_text(&m.blocks);
            let tool_calls: Vec<WireOutToolCall> = m
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::ToolUse {
                        id, name, input_json, ..
                    } => Some(WireOutToolCall {
                        id: id.clone(),
                        kind: "function",
                        function: WireOutToolFunction {
                            name: name.clone(),
                            arguments: String::from_utf8(input_json.clone()).unwrap_or_default(),
                        },
                    }),
                    _ => None,
                })
                .collect();
            let content = if text.is_empty() { None } else { Some(text) };
            let tc = if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            };
            out.push(WireMessage {
                role: "assistant",
                content,
                tool_call_id: None,
                tool_calls: tc,
            });
        }
        Role::Tool => {
            for b in &m.blocks {
                if let Block::ToolResult {
                    tool_use_id, inline, ..
                } = b
                {
                    let content_str = inline
                        .as_deref()
                        .and_then(|v| std::str::from_utf8(v).ok())
                        .unwrap_or("")
                        .to_string();
                    out.push(WireMessage {
                        role: "tool",
                        content: Some(content_str),
                        tool_call_id: Some(tool_use_id.clone()),
                        tool_calls: None,
                    });
                }
            }
        }
    }
}

fn collect_text(blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        if let Block::Text { text, .. } = b {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    out
}
