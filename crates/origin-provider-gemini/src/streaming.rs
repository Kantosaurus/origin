//! Gemini SSE → `TokenEvent` adapter.
//!
//! Consumes the shared `origin_provider::sse::from_reqwest` event stream and
//! emits canonical token events into a `Ring`. Each Gemini SSE `data:` payload
//! is a JSON object with the same shape as a non-streaming response, but with
//! partial `parts` (usually a single text-delta or one functionCall fragment).

use futures_util::StreamExt;
use origin_provider::sse;
use origin_provider::ProviderError;
use origin_stream::{Ring, TokenEvent, TokenKind};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct WireStreamChunk {
    #[serde(default)]
    candidates: Vec<WireStreamCandidate>,
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

    // TODO(P8.x): Gemini reports `usageMetadata` only on the final SSE frame
    // (and sometimes omits it entirely). Parse the final-frame `usageMetadata`
    // and publish a `TokenKind::Usage` event with token counts — mirrors the
    // OpenAI streaming TODO.
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

        for cand in chunk.candidates {
            for part in cand.content.parts {
                if let Some(text) = part.text {
                    if !text.is_empty() {
                        ring.publish(&TokenEvent::new(TokenKind::TextDelta, text.into_bytes()))
                            .map_err(|e| ProviderError::Api(e.to_string()))?;
                    }
                }
                if let Some(fc) = part.function_call {
                    if let Some(name) = fc.name.as_ref() {
                        let id = format!("call_{name}");
                        let payload = [id.as_bytes(), b"\0", name.as_bytes()].concat();
                        ring.publish(&TokenEvent::new(TokenKind::ToolUseStart, payload))
                            .map_err(|e| ProviderError::Api(e.to_string()))?;
                    }
                    if let Some(args) = fc.args.as_ref() {
                        let bytes = serde_json::to_vec(args)
                            .map_err(|e| ProviderError::Api(format!("args json: {e}")))?;
                        ring.publish(&TokenEvent::new(TokenKind::ToolUseDelta, bytes))
                            .map_err(|e| ProviderError::Api(e.to_string()))?;
                    }
                }
            }
            if cand.finish_reason.is_some() {
                ring.publish(&TokenEvent::new(TokenKind::TurnEnd, Vec::new()))
                    .map_err(|e| ProviderError::Api(e.to_string()))?;
                return Ok(());
            }
        }
    }
    Ok(())
}
