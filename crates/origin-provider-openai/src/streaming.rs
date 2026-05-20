//! `OpenAI` SSE → `TokenEvent` adapter.
//!
//! Consumes the shared `origin_provider::sse::from_reqwest` event stream and
//! emits canonical token events into a `Ring`.

use futures_util::StreamExt;
use origin_provider::sse;
use origin_provider::ProviderError;
use origin_stream::{Ring, TokenEvent, TokenKind};
use serde::Deserialize;

#[derive(Deserialize)]
struct WireStreamChunk {
    #[serde(default)]
    choices: Vec<WireStreamChoice>,
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
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<WireStreamToolFnDelta>,
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

        for choice in chunk.choices {
            // TODO(P8.x): track `index` field on streaming tool_calls to support concurrent
            // tool-call demux. Currently consumers cannot disambiguate which tool a
            // ToolUseDelta belongs to when OpenAI interleaves multiple tool calls.
            // tool_use_start: a tool-call delta arriving with a fresh id+name.
            if let Some(tcs) = &choice.delta.tool_calls {
                for tc in tcs {
                    if let (Some(id), Some(func)) = (tc.id.as_ref(), tc.function.as_ref()) {
                        if let Some(name) = func.name.as_ref() {
                            let payload = [id.as_bytes(), b"\0", name.as_bytes()].concat();
                            ring.publish(&TokenEvent::new(TokenKind::ToolUseStart, payload))
                                .map_err(|e| ProviderError::Api(e.to_string()))?;
                        }
                    }
                    if let Some(args) = tc.function.as_ref().and_then(|f| f.arguments.as_ref()) {
                        ring.publish(&TokenEvent::new(
                            TokenKind::ToolUseDelta,
                            args.clone().into_bytes(),
                        ))
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
                // TODO(P8.x): set `stream_options.include_usage = true` in the request body and
                // parse the final `usage` SSE frame to publish a `TokenKind::Usage` event with
                // token counts. OpenAI omits usage from streaming responses by default.
                ring.publish(&TokenEvent::new(TokenKind::TurnEnd, Vec::new()))
                    .map_err(|e| ProviderError::Api(e.to_string()))?;
            }
        }
    }
    Ok(())
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
