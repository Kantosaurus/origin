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
