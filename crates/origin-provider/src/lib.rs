//! Provider trait + canonical request/response/usage/error types.
//!
//! Each `Provider` implementation translates the canonical types to its wire
//! format. Multi-provider support is achieved by per-provider crates that
//! `impl Provider for …`.

pub mod catalog;
mod catalog_rows;
pub use catalog_rows::builtin_catalog;
pub mod custom;
pub mod ndjson;
pub mod openai_tools;
pub mod sse;

#[cfg(feature = "recorder")]
pub mod recorder_hook {
    //! Global `ProviderTap` registered by the daemon at startup. Concrete
    //! provider impls call `tap()` and feed frames through it without
    //! changing their public APIs.

    use origin_replay::provider_tap::ProviderTap;
    use std::sync::Arc;

    static TAP: parking_lot::RwLock<Option<Arc<ProviderTap>>> = parking_lot::RwLock::new(None);

    pub fn register_tap(tap: Arc<ProviderTap>) {
        *TAP.write() = Some(tap);
    }

    #[must_use]
    pub fn tap() -> Option<Arc<ProviderTap>> {
        TAP.read().clone()
    }
}

use origin_core::types::Message;
use thiserror::Error;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_input_tokens: u32,
    pub cache_creation_input_tokens: u32,
}

#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema describing the tool input shape.
    pub input_schema_json: String,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub model: String,
    pub tools: Vec<ToolSchema>,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub assistant: Message,
    pub usage: Usage,
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("api: {0}")]
    Api(String),
    #[error("auth")]
    Auth,
    #[error("rate limit; retry after {retry_after_secs}s")]
    RateLimit { retry_after_secs: u32, message: String },
}

/// Inflate any CAS-handle-backed `ToolResult` blocks into inline bytes.
///
/// The daemon stores every tool result as a CAS handle (`handle: Some`,
/// `inline: None`) whenever a CAS is configured (which is always in production).
/// Each provider's wire encoder only understands inline content, so a provider
/// must inflate handles back into bytes before encoding — otherwise the model
/// receives empty tool results on every follow-up turn and the agentic loop
/// silently breaks. The Anthropic provider does this with extra Plan-aware
/// `Reference` downgrading; every other provider can use this plain helper.
///
/// Returns a cloned message vector with handles resolved. Mirrors the Anthropic
/// provider's loud-failure contract: a handle with no CAS configured, or a CAS
/// miss, is an error rather than a silently-empty result.
///
/// # Errors
/// Returns [`ProviderError::Api`] if a handle is present but no CAS is
/// configured, or if the CAS lookup fails or misses.
pub fn inflate_tool_result_handles(
    messages: &[Message],
    cas: Option<&std::sync::Arc<origin_cas::Store>>,
) -> Result<Vec<Message>, ProviderError> {
    use origin_core::types::Block;
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        let mut nm = m.clone();
        for b in &mut nm.blocks {
            if let Block::ToolResult { handle, inline, .. } = b {
                if inline.is_none() {
                    if let Some(h) = handle.take() {
                        let store = cas.ok_or_else(|| {
                            ProviderError::Api("ToolResult handle present but no CAS configured".into())
                        })?;
                        let bytes = store
                            .get(origin_cas::Hash::from_bytes(h))
                            .map_err(|e| ProviderError::Api(format!("cas get: {e}")))?
                            .ok_or_else(|| ProviderError::Api("cas miss for tool result handle".into()))?;
                        *inline = Some(bytes);
                    }
                }
            }
        }
        out.push(nm);
    }
    Ok(out)
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;

    /// Send a single non-streaming chat request.
    ///
    /// # Errors
    /// Returns `ProviderError` for transport, API, auth, or rate-limit failures.
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError>;

    /// Stream tokens into `ring`. Default impl falls back to `chat` and emits
    /// one `TextDelta` + `TurnEnd` so providers without native streaming still
    /// work behind the ring API.
    ///
    /// # Errors
    /// Returns `ProviderError` for transport, API, auth, or rate-limit failures.
    async fn chat_stream(&self, req: ChatRequest, ring: &origin_stream::Ring) -> Result<(), ProviderError> {
        let resp = self.chat(req).await?;
        let text: String = resp
            .assistant
            .blocks
            .iter()
            .filter_map(|b| match b {
                origin_core::types::Block::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        ring.publish(&origin_stream::TokenEvent::new(
            origin_stream::TokenKind::TextDelta,
            text.into_bytes(),
        ))
        .map_err(|e| ProviderError::Api(e.to_string()))?;
        ring.publish(&origin_stream::TokenEvent::new(
            origin_stream::TokenKind::TurnEnd,
            Vec::new(),
        ))
        .map_err(|e| ProviderError::Api(e.to_string()))?;
        ring.close();
        Ok(())
    }
}
