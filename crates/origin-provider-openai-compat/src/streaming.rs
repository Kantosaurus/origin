// SPDX-License-Identifier: Apache-2.0
//! `OpenAI` SSE → `TokenEvent` adapter.
//!
//! Consumes the shared `origin_provider::sse::from_reqwest` event stream and
//! emits canonical token events into a `Ring`.

use futures_util::StreamExt;
use origin_provider::sse;
use origin_provider::{ProviderError, Usage};
use origin_stream::{Ring, TokenEvent, TokenKind};
use serde::Deserialize;

#[derive(Deserialize)]
struct WireStreamChunk {
    #[serde(default)]
    choices: Vec<WireStreamChoice>,
    #[serde(default)]
    usage: Option<WireStreamUsage>,
}

#[derive(Deserialize)]
struct WireStreamChoice {
    #[serde(default)]
    delta: WireStreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireStreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireStreamToolCallDelta>>,
}

#[derive(Deserialize)]
struct WireStreamToolCallDelta {
    /// Position of this tool call within the assistant's response. Required
    /// to demux concurrent tool calls — `id` and `name` arrive only on the
    /// first fragment per index, while later fragments only carry `arguments`.
    #[serde(default)]
    index: Option<u32>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<WireStreamToolFnDelta>,
}

#[derive(Deserialize, Default, Clone, Copy)]
struct WireStreamUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

#[derive(Deserialize)]
struct WireStreamToolFnDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// Drive the SSE stream from `resp` and publish into `ring`.
///
/// Stops when a `data: [DONE]` sentinel is received or the stream ends. Does
/// not call `ring.close()` — the caller controls ring lifecycle.
///
/// # Errors
/// Returns `ProviderError::Api` on JSON or ring failures and propagates
/// `ProviderError` from the SSE pump.
pub async fn parse_into_ring(resp: reqwest::Response, ring: &Ring) -> Result<(), ProviderError> {
    let stream = sse::from_reqwest(resp);
    pin_utils::pin_mut!(stream);

    while let Some(item) = stream.next().await {
        let ev = item?;
        let raw = ev.data;
        if raw.is_empty() {
            continue;
        }
        if raw.trim() == "[DONE]" {
            break;
        }

        let chunk: WireStreamChunk = match serde_json::from_str(&raw) {
            Ok(c) => c,
            Err(e) => return Err(ProviderError::Api(format!("sse json: {e}; raw={raw}"))),
        };

        // OpenAI emits `usage` on a trailing frame (with empty `choices`) when
        // the request set `stream_options.include_usage=true`. Forward the
        // counts whenever they appear so consumers can record real usage.
        if let Some(u) = chunk.usage {
            ring.publish(&TokenEvent::new(TokenKind::Usage, encode_usage(u)))
                .map_err(|e| ProviderError::Api(e.to_string()))?;
        }

        for choice in chunk.choices {
            // Each fragment must carry the `tc.index` from the wire so the
            // daemon can demux interleaved parallel tool calls back to the
            // right tool. The payload shapes match the Anthropic provider:
            //   ToolUseStart: 4-byte LE index + id + b'\0' + name
            //   ToolUseDelta: 4-byte LE index + partial JSON bytes
            //
            // `tc.index` is required by the OpenAI spec, but non-strict
            // compat servers occasionally omit it; fall back to the position
            // within this fragment so we still emit valid (if degenerate)
            // demux bytes rather than silently corrupting downstream state.
            if let Some(tcs) = &choice.delta.tool_calls {
                for (pos, tc) in tcs.iter().enumerate() {
                    let index = tc.index.unwrap_or_else(|| u32::try_from(pos).unwrap_or(0));
                    let idx_bytes = index.to_le_bytes();
                    if let (Some(id), Some(func)) = (tc.id.as_ref(), tc.function.as_ref()) {
                        if let Some(name) = func.name.as_ref() {
                            let mut payload = Vec::with_capacity(4 + id.len() + 1 + name.len());
                            payload.extend_from_slice(&idx_bytes);
                            payload.extend_from_slice(id.as_bytes());
                            payload.push(b'\0');
                            payload.extend_from_slice(name.as_bytes());
                            ring.publish(&TokenEvent::new(TokenKind::ToolUseStart, payload))
                                .map_err(|e| ProviderError::Api(e.to_string()))?;
                        }
                    }
                    if let Some(args) = tc.function.as_ref().and_then(|f| f.arguments.as_ref()) {
                        let mut payload = Vec::with_capacity(4 + args.len());
                        payload.extend_from_slice(&idx_bytes);
                        payload.extend_from_slice(args.as_bytes());
                        ring.publish(&TokenEvent::new(TokenKind::ToolUseDelta, payload))
                            .map_err(|e| ProviderError::Api(e.to_string()))?;
                    }
                }
            }
            if let Some(text) = choice.delta.content {
                if !text.is_empty() {
                    ring.publish(&TokenEvent::new(TokenKind::TextDelta, text.into_bytes()))
                        .map_err(|e| ProviderError::Api(e.to_string()))?;
                }
            }
            if choice.finish_reason.is_some() {
                ring.publish(&TokenEvent::new(TokenKind::TurnEnd, Vec::new()))
                    .map_err(|e| ProviderError::Api(e.to_string()))?;
            }
        }
    }
    Ok(())
}

fn encode_usage(u: WireStreamUsage) -> Vec<u8> {
    // 4 × u32 BE. Order: input, output, cache_read, cache_creation. OpenAI
    // does not report cache token counts on Chat Completions streams.
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&u.prompt_tokens.to_be_bytes());
    out.extend_from_slice(&u.completion_tokens.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out
}

/// Test-only view of a parsed `OpenAI` SSE frame.
///
/// Exposes the `index` field on the first observed `tool_call` delta and any
/// `usage` counts on the trailing frame (only present when
/// `stream_options.include_usage=true`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TestFrame {
    pub index: Option<u32>,
    pub usage: Option<Usage>,
}

/// Parse a single SSE `data: …` line into a `TestFrame`. Returns `None` for
/// non-data lines, blank payloads, `[DONE]`, or invalid JSON.
#[must_use]
pub fn parse_chunk_for_test(line: &[u8]) -> Option<TestFrame> {
    let text = std::str::from_utf8(line).ok()?;
    let payload = text
        .trim_start()
        .strip_prefix("data:")
        .map_or(text.trim(), str::trim_start)
        .trim();
    if payload.is_empty() || payload == "[DONE]" {
        return None;
    }
    let chunk: WireStreamChunk = serde_json::from_str(payload).ok()?;
    let index = chunk
        .choices
        .iter()
        .find_map(|c| c.delta.tool_calls.as_ref())
        .and_then(|tcs| tcs.iter().find_map(|tc| tc.index));
    let usage = chunk.usage.map(|u| Usage {
        input_tokens: u.prompt_tokens,
        output_tokens: u.completion_tokens,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
    });
    Some(TestFrame { index, usage })
}

/// Panic-free synchronous SSE wire-chunk decoder for fuzz targets.
///
/// Splits the input on SSE event boundaries (blank lines), strips a
/// leading `data:` prefix on each chunk, and feeds each payload through
/// the same `WireStreamChunk` JSON decoder used by the live streaming
/// path. The `[DONE]` sentinel is recognized and skipped. This function
/// MUST NOT panic on arbitrary input.
///
/// # Errors
/// `ParseError::Empty` if the input contains no candidate JSON payloads.
/// `ParseError::Invalid` if a candidate payload fails to deserialize.
pub fn parse(bytes: &[u8]) -> Result<(), ParseError> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Err(ParseError::Invalid);
    };
    let mut saw_any = false;
    for chunk in text.split("\n\n") {
        let mut payload = String::new();
        for line in chunk.lines() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("data:") {
                payload.push_str(rest.trim_start());
            }
        }
        if payload.is_empty() {
            continue;
        }
        if payload.trim() == "[DONE]" {
            saw_any = true;
            continue;
        }
        saw_any = true;
        if serde_json::from_str::<WireStreamChunk>(&payload).is_err() {
            return Err(ParseError::Invalid);
        }
    }
    if saw_any {
        Ok(())
    } else {
        Err(ParseError::Empty)
    }
}

/// Reasons `parse` may reject input.
#[derive(Debug)]
pub enum ParseError {
    /// No `data:` payloads were found in the byte stream.
    Empty,
    /// A `data:` payload failed to deserialize as a wire chunk.
    Invalid,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("no SSE payloads found"),
            Self::Invalid => f.write_str("invalid wire chunk"),
        }
    }
}

impl std::error::Error for ParseError {}
