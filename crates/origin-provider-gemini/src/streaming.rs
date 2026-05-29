//! Gemini SSE → `TokenEvent` adapter.
//!
//! Consumes the shared `origin_provider::sse::from_reqwest` event stream and
//! emits canonical token events into a `Ring`. Each Gemini SSE `data:` payload
//! is a JSON object with the same shape as a non-streaming response, but with
//! partial `parts` (usually a single text-delta or one functionCall fragment).

use futures_util::StreamExt;
use origin_provider::sse;
use origin_provider::{ProviderError, Usage};
use origin_stream::{Ring, TokenEvent, TokenKind};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct WireStreamChunk {
    #[serde(default)]
    candidates: Vec<WireStreamCandidate>,
    #[serde(rename = "usageMetadata", default)]
    usage_metadata: Option<WireUsageMetadata>,
}

#[allow(clippy::struct_field_names)]
#[derive(Deserialize, Default, Clone, Copy)]
struct WireUsageMetadata {
    #[serde(rename = "promptTokenCount", default)]
    prompt_token_count: u32,
    #[serde(rename = "candidatesTokenCount", default)]
    candidates_token_count: u32,
    #[serde(rename = "cachedContentTokenCount", default)]
    cached_content_token_count: u32,
}

#[derive(Deserialize)]
struct WireStreamCandidate {
    #[serde(default)]
    content: WireStreamContent,
    #[serde(rename = "finishReason", default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireStreamContent {
    #[serde(default)]
    parts: Vec<WireStreamPart>,
}

#[derive(Deserialize)]
struct WireStreamPart {
    #[serde(default)]
    text: Option<String>,
    #[serde(rename = "functionCall", default)]
    function_call: Option<WireStreamFnCall>,
}

#[derive(Deserialize)]
struct WireStreamFnCall {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    args: Option<Value>,
}

/// Drive the SSE stream from `resp` and publish into `ring`.
///
/// Stops when the stream ends or a `finishReason` is observed. Does not call
/// `ring.close()` — the caller controls ring lifecycle.
///
/// # Errors
/// Returns `ProviderError::Api` on JSON or ring failures and propagates
/// `ProviderError` from the SSE pump.
pub async fn parse_into_ring(resp: reqwest::Response, ring: &Ring) -> Result<(), ProviderError> {
    let stream = sse::from_reqwest(resp);
    pin_utils::pin_mut!(stream);

    // Per-turn counter for synthesised tool-use ids. Gemini's wire format
    // does not carry an id for `functionCall` parts, so we mint one — but
    // if the same `name` appears twice in a turn (legitimate for parallel
    // calls), `call_<name>` alone collides. Suffixing with the call's
    // position within the turn keeps every id unique.
    let mut tool_call_idx: u32 = 0;

    while let Some(item) = stream.next().await {
        let ev = item?;
        let raw = ev.data;
        if raw.is_empty() {
            continue;
        }

        let chunk: WireStreamChunk = match serde_json::from_str(&raw) {
            Ok(c) => c,
            Err(e) => return Err(ProviderError::Api(format!("sse json: {e}; raw={raw}"))),
        };

        // Gemini reports `usageMetadata` on the final SSE frame (and sometimes
        // on intermediate frames too). Publish a `Usage` token event whenever
        // present so consumers can record real token counts for streaming.
        let usage_meta = chunk.usage_metadata;
        let mut finished = false;
        for cand in chunk.candidates {
            for part in cand.content.parts {
                if let Some(text) = part.text {
                    if !text.is_empty() {
                        ring.publish(&TokenEvent::new(TokenKind::TextDelta, text.into_bytes()))
                            .map_err(|e| ProviderError::Api(e.to_string()))?;
                    }
                }
                if let Some(fc) = part.function_call {
                    // The daemon decoder keys tool-call start/delta events on a
                    // 4-byte LE index prefix (see decode_tool_use_start /
                    // decode_tool_use_delta in origin-daemon). Use one stable
                    // index per function_call so the start and its args delta
                    // correlate; without the prefix the index is read from the
                    // id/JSON bytes (garbage) and the delta orphans, silently
                    // dropping the tool arguments.
                    let idx = tool_call_idx;
                    tool_call_idx = tool_call_idx.saturating_add(1);
                    if let Some(name) = fc.name.as_ref() {
                        let id = format!("call_{name}_{idx}");
                        let mut payload = idx.to_le_bytes().to_vec();
                        payload.extend_from_slice(id.as_bytes());
                        payload.push(b'\0');
                        payload.extend_from_slice(name.as_bytes());
                        ring.publish(&TokenEvent::new(TokenKind::ToolUseStart, payload))
                            .map_err(|e| ProviderError::Api(e.to_string()))?;
                    }
                    if let Some(args) = fc.args.as_ref() {
                        let json = serde_json::to_vec(args)
                            .map_err(|e| ProviderError::Api(format!("args json: {e}")))?;
                        let mut payload = idx.to_le_bytes().to_vec();
                        payload.extend_from_slice(&json);
                        ring.publish(&TokenEvent::new(TokenKind::ToolUseDelta, payload))
                            .map_err(|e| ProviderError::Api(e.to_string()))?;
                    }
                }
            }
            if cand.finish_reason.is_some() {
                finished = true;
            }
        }
        if let Some(um) = usage_meta {
            ring.publish(&TokenEvent::new(TokenKind::Usage, encode_usage_meta(um)))
                .map_err(|e| ProviderError::Api(e.to_string()))?;
        }
        if finished {
            ring.publish(&TokenEvent::new(TokenKind::TurnEnd, Vec::new()))
                .map_err(|e| ProviderError::Api(e.to_string()))?;
            return Ok(());
        }
    }
    Ok(())
}

fn encode_usage_meta(u: WireUsageMetadata) -> Vec<u8> {
    // 4 × u32 BE. Order: input, output, cache_read, cache_creation.
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&u.prompt_token_count.to_be_bytes());
    out.extend_from_slice(&u.candidates_token_count.to_be_bytes());
    out.extend_from_slice(&u.cached_content_token_count.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out
}

/// Test-only view of a parsed Gemini SSE frame, exposing the optional
/// `usageMetadata` token counts. Returned by `parse_chunk_for_test`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TestFrame {
    pub usage: Option<Usage>,
    pub finished: bool,
}

/// Parse a single SSE `data: …` line into a `TestFrame`. Returns `None` for
/// non-data lines, blank payloads, or invalid JSON.
#[must_use]
pub fn parse_chunk_for_test(line: &[u8]) -> Option<TestFrame> {
    let text = std::str::from_utf8(line).ok()?;
    let payload = text
        .trim_start()
        .strip_prefix("data:")
        .map_or(text.trim(), str::trim_start)
        .trim();
    if payload.is_empty() {
        return None;
    }
    let chunk: WireStreamChunk = serde_json::from_str(payload).ok()?;
    let finished = chunk.candidates.iter().any(|c| c.finish_reason.is_some());
    let usage = chunk.usage_metadata.map(|u| Usage {
        input_tokens: u.prompt_token_count,
        output_tokens: u.candidates_token_count,
        cache_read_input_tokens: u.cached_content_token_count,
        cache_creation_input_tokens: 0,
    });
    Some(TestFrame { usage, finished })
}
