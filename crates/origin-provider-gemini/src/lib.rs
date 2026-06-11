// SPDX-License-Identifier: Apache-2.0
//! Gemini provider (non-streaming + SSE streaming).
//!
//! Implements `origin_provider::Provider` against Google's Generative Language
//! REST API: `POST {base}/v1beta/models/{model}:generateContent?key={api_key}`
//! for non-streaming and `:streamGenerateContent?key={key}&alt=sse` for SSE.
//! Streaming reuses the shared `origin_provider::sse` pump.

pub mod streaming;
mod wire;

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use reqwest::StatusCode;
use std::sync::Arc;

const DEFAULT_BASE: &str = "https://generativelanguage.googleapis.com";

/// Controls which auth mechanism is used for Gemini requests.
enum AuthKind {
    /// `?key=<api_key>` query parameter — the standard Gemini API key path.
    ApiKey(String),
    /// `Authorization: Bearer <token>` header — used when a refreshed OAuth
    /// token is presented (e.g. Gemini CLI OAuth flow).
    OAuthBearer(String),
}

/// Gemini provider backed by `generateContent` / `streamGenerateContent`.
///
/// The API key is embedded as a `?key=` query parameter, not a header. The
/// struct deliberately does not derive `Debug` so the key cannot be logged.
pub struct Gemini {
    auth: AuthKind,
    base: String,
    client: reqwest::Client,
    cas: Option<Arc<origin_cas::Store>>,
}

impl Gemini {
    /// Construct with the default base URL.
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE)
    }

    /// Construct against an arbitrary base URL (for testing and gateways).
    #[must_use]
    pub fn with_base_url(api_key: impl Into<String>, base: &str) -> Self {
        Self {
            auth: AuthKind::ApiKey(api_key.into()),
            base: base.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
            cas: None,
        }
    }

    /// Construct using a refreshed OAuth bearer token instead of an API key.
    ///
    /// Sends `Authorization: Bearer <token>` and omits the `?key=` query
    /// parameter from the URL.
    #[must_use]
    pub fn with_oauth_bearer(token: impl Into<String>) -> Self {
        Self {
            auth: AuthKind::OAuthBearer(token.into()),
            base: DEFAULT_BASE.to_string(),
            client: reqwest::Client::new(),
            cas: None,
        }
    }

    /// Attach the content-addressed store so handle-backed `ToolResult` blocks
    /// are inflated to inline bytes before wire encoding. Without this, the
    /// daemon's handle-backed tool results encode as empty `functionResponse`
    /// content and the model never sees what its tools returned.
    #[must_use]
    pub fn with_cas(mut self, cas: Arc<origin_cas::Store>) -> Self {
        self.cas = Some(cas);
        self
    }

    fn url(&self, model: &str, action: &str, extra_query: &str) -> String {
        // `extra_query` is either "" or "&alt=sse".
        match &self.auth {
            AuthKind::ApiKey(key) => format!(
                "{}/v1beta/models/{}:{}?key={}{}",
                self.base, model, action, key, extra_query
            ),
            AuthKind::OAuthBearer(_) => {
                // For OAuth, the key goes in the Authorization header instead.
                // extra_query may start with "&" — replace it with "?" when it
                // is the first query param.
                let qs = extra_query.trim_start_matches('&');
                if qs.is_empty() {
                    format!("{}/v1beta/models/{}:{}", self.base, model, action)
                } else {
                    format!("{}/v1beta/models/{}:{}?{}", self.base, model, action, qs)
                }
            }
        }
    }

    /// Apply the appropriate auth header to a request builder (no-op for `ApiKey`,
    /// which uses the `?key=` query parameter instead).
    fn apply_auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            AuthKind::ApiKey(_) => builder,
            AuthKind::OAuthBearer(token) => builder.header("Authorization", format!("Bearer {token}")),
        }
    }
}

#[async_trait]
impl Provider for Gemini {
    fn name(&self) -> &'static str {
        "gemini"
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let messages = origin_provider::inflate_tool_result_handles(&req.messages, self.cas.as_ref())?;
        let req = ChatRequest { messages, ..req };
        let body = wire::encode_request(&req);
        let url = self.url(&req.model, "generateContent", "");
        let resp = self
            .apply_auth(self.client.post(&url))
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        if resp.status() == StatusCode::OK {
            let wire: wire::WireResponse = resp
                .json()
                .await
                .map_err(|e| ProviderError::Api(format!("decode: {e}")))?;
            return Ok(decode_response(wire));
        }
        Err(status_error(resp).await)
    }

    async fn chat_stream(&self, req: ChatRequest, ring: &origin_stream::Ring) -> Result<(), ProviderError> {
        let messages = origin_provider::inflate_tool_result_handles(&req.messages, self.cas.as_ref())?;
        let req = ChatRequest { messages, ..req };
        let body = wire::encode_request(&req);
        let url = self.url(&req.model, "streamGenerateContent", "&alt=sse");
        let resp = self
            .apply_auth(self.client.post(&url))
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            let err = status_error(resp).await;
            ring.close();
            return Err(err);
        }

        let result = streaming::parse_into_ring(resp, ring).await;
        ring.close();
        result
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

fn decode_response(wire: wire::WireResponse) -> ChatResponse {
    let mut blocks: Vec<Block> = Vec::new();
    if let Some(candidate) = wire.candidates.into_iter().next() {
        for part in candidate.content.parts {
            if let Some(text) = part.text {
                if !text.is_empty() {
                    blocks.push(Block::Text {
                        text,
                        cache_marker: None,
                    });
                }
            }
            if let Some(fc) = part.function_call {
                let input_json = serde_json::to_vec(&fc.args).unwrap_or_default();
                // Pack Gemini's real call id as `name#id` so the tool-result
                // encoder can echo it back (correct mapping for parallel
                // same-name calls); fall back to `call_<name>` when absent.
                let id = fc.id.map_or_else(
                    || format!("call_{}", fc.name),
                    |real| format!("{}#{real}", fc.name),
                );
                blocks.push(Block::ToolUse {
                    id,
                    name: fc.name,
                    input_json,
                    cache_marker: None,
                });
            }
        }
    }

    let assistant = Message {
        role: Role::Assistant,
        blocks,
    };
    let usage = Usage {
        input_tokens: wire.usage_metadata.prompt_token_count,
        output_tokens: wire.usage_metadata.candidates_token_count,
        cache_read_input_tokens: wire.usage_metadata.cached_content_token_count,
        cache_creation_input_tokens: 0,
    };
    ChatResponse { assistant, usage }
}
