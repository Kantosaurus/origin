// SPDX-License-Identifier: Apache-2.0
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

/// Reasoning-effort level for a turn.
///
/// Mirrors claude-code's `/effort` slider plus the dedicated `/fast` mode.
/// Threaded onto [`ChatRequest`] as `Option<ReasoningEffort>`; the default of
/// `None` means "unspecified" and leaves every provider's wire output
/// byte-identical to before this field existed. A provider that does not
/// understand effort simply ignores the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEffort {
    /// Lowest latency; minimal deliberation (`/fast`).
    Fast,
    /// Low effort.
    Low,
    /// Balanced effort (the usual middle setting).
    Medium,
    /// High effort; more deliberation.
    High,
    /// Maximum effort; deepest deliberation.
    Max,
    /// `Ultracode`: the deepest tier — maximum reasoning *plus* always-on
    /// multi-agent swarm orchestration. On the provider wire it is the highest
    /// valid reasoning level (Anthropic `max`); the swarm-orchestration half is
    /// realised by the always-on `Task` surface, not the wire. This is the
    /// default effort for Anthropic models.
    Ultracode,
}

impl ReasoningEffort {
    /// The canonical lowercase wire token for this level.
    ///
    /// This is the string emitted on the provider wire when a level is set.
    #[must_use]
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
            Self::Ultracode => "ultracode",
        }
    }

    /// The Anthropic Messages API `output_config.effort` value for this level.
    ///
    /// Anthropic accepts only `low|medium|high|xhigh|max` — there is no `fast`,
    /// so [`Self::Fast`] maps to the closest valid low-latency level, `low`.
    /// `Ultracode` is an origin-internal tier (the wire has no such value); it
    /// maps to the highest valid Anthropic level, `max`.
    #[must_use]
    pub const fn as_anthropic_effort(self) -> &'static str {
        match self {
            Self::Fast | Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max | Self::Ultracode => "max",
        }
    }

    /// The `OpenAI` `reasoning_effort` value for this level.
    ///
    /// `OpenAI` reasoning models accept `low|medium|high` universally (`minimal`
    /// and `xhigh` are model-specific). `fast` and `max` are not valid values,
    /// so [`Self::Fast`] maps to `low` and [`Self::Max`]/[`Self::Ultracode`] to
    /// `high`.
    #[must_use]
    pub const fn as_openai_effort(self) -> &'static str {
        match self {
            Self::Fast | Self::Low => "low",
            Self::Medium => "medium",
            Self::High | Self::Max | Self::Ultracode => "high",
        }
    }

    /// Parse a canonical wire token (`fast`/`low`/`medium`/`high`/`max`,
    /// case-insensitive) back into a level. Returns `None` for anything else
    /// so callers can leave the wire byte-identical when the token is unknown.
    #[must_use]
    pub fn from_wire_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "fast" => Some(Self::Fast),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "max" => Some(Self::Max),
            "ultracode" => Some(Self::Ultracode),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub model: String,
    pub tools: Vec<ToolSchema>,
    /// Optional reasoning-effort hint for this turn.
    ///
    /// `None` (the default) leaves the provider wire byte-identical to the
    /// pre-effort behavior; a `Some(level)` causes effort-aware providers to
    /// emit the level on the wire.
    pub effort: Option<ReasoningEffort>,
    /// Multimodal attachments (images/PDF pages) to append to the user turn
    /// (item G-live).
    ///
    /// Empty by default, which leaves the provider wire byte-identical to the
    /// text-only behavior. When non-empty, an attachment-aware provider encoder
    /// translates each block via the `origin-multimodal` `encode_*_block`
    /// helpers and appends it to the final user message's content.
    pub attachments: Vec<origin_multimodal::ContentBlock>,
    /// Optional extended-thinking budget (in tokens) for this turn.
    ///
    /// `None` (the default) leaves every provider's wire byte-identical to the
    /// pre-thinking-tokens behavior. `Some(n)` asks an extended-thinking-aware
    /// provider (today: Anthropic) to enable interleaved/extended thinking with
    /// a `budget_tokens` of `n`; the encoder also guarantees the top-level
    /// `max_tokens` exceeds `n`, per Anthropic's requirement. Providers that do
    /// not support an explicit thinking budget (e.g. `OpenAI`-compatible) ignore
    /// the value entirely. *Closes: aider `--thinking-tokens`.*
    pub thinking_tokens: Option<u32>,
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

/// Substituted in place of a tool result whose CAS payload is missing.
///
/// A handle can dangle when the daemon was killed/crashed before its Hot-tier
/// CAS bytes were flushed (e.g. a SIGKILL auto-update restart), while the
/// persisted transcript still references the handle. Rather than fail the whole
/// turn with a fatal "cas miss", the providers inline this notice — keeping the
/// `tool_use`/`tool_result` pairing intact (no Anthropic 400) so the
/// conversation continues, and telling the model plainly that the output is gone.
pub const CAS_MISS_PLACEHOLDER: &str =
    "[tool result unavailable: its cached output was lost when the agent daemon restarted]";

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
/// Returns a cloned message vector with handles resolved. A handle present with
/// no CAS configured is still a hard error (a misconfiguration). A CAS *miss*,
/// however, degrades gracefully: the result is replaced with
/// [`CAS_MISS_PLACEHOLDER`] (the payload was lost in a daemon restart) so the
/// turn continues instead of failing the whole loop.
///
/// # Errors
/// Returns [`ProviderError::Api`] if a handle is present but no CAS is
/// configured, or if the CAS lookup itself errors (a miss does not error).
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
                            .unwrap_or_else(|| {
                                tracing::warn!(
                                    "cas miss for tool result handle; substituting placeholder \
                                     (cached output lost across a daemon restart)"
                                );
                                CAS_MISS_PLACEHOLDER.as_bytes().to_vec()
                            });
                        *inline = Some(bytes);
                    }
                }
            }
        }
        out.push(nm);
    }
    // Tool-pairing repair at the non-anthropic wire choke (parity with the
    // anthropic encoder): drop an orphaned tool_result and a trailing orphan
    // tool_use so a resumed/corrupted/interrupted transcript can't 400 the
    // OpenAI-compatible (or other) provider. Idempotent on a well-formed
    // transcript.
    let out = origin_core::types::strip_orphan_tool_results(out);
    Ok(origin_core::types::strip_trailing_orphan_tool_use(out))
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;

    /// The provider's upstream base URL, when statically known (e.g. an
    /// OpenAI-compatible endpoint). Used to populate the OpenTelemetry
    /// `server.address` / `server.port` `gen_ai` attributes (otel feature only).
    /// The default is `None` — providers whose endpoint is fixed/opaque simply
    /// omit those attributes, leaving the span byte-identical.
    fn base_url(&self) -> Option<&str> {
        None
    }

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

#[cfg(test)]
mod effort_tests {
    use super::ReasoningEffort;

    #[test]
    fn ultracode_round_trips_its_wire_token() {
        assert_eq!(ReasoningEffort::Ultracode.as_wire_str(), "ultracode");
        assert_eq!(
            ReasoningEffort::from_wire_str("ultracode"),
            Some(ReasoningEffort::Ultracode)
        );
        assert_eq!(
            ReasoningEffort::from_wire_str("ULTRACODE"),
            Some(ReasoningEffort::Ultracode)
        );
    }

    #[test]
    fn ultracode_maps_to_the_highest_valid_provider_levels() {
        // On the Anthropic wire `ultracode` has no literal value; it must encode
        // as the highest VALID level (`max`) or the Messages API 400s.
        assert_eq!(ReasoningEffort::Ultracode.as_anthropic_effort(), "max");
        // OpenAI tops out at `high`.
        assert_eq!(ReasoningEffort::Ultracode.as_openai_effort(), "high");
    }

    #[test]
    fn every_level_round_trips_its_wire_token() {
        for lvl in [
            ReasoningEffort::Fast,
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Max,
            ReasoningEffort::Ultracode,
        ] {
            assert_eq!(ReasoningEffort::from_wire_str(lvl.as_wire_str()), Some(lvl));
        }
    }
}
