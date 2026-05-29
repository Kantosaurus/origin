//! Ollama NDJSON → `TokenEvent` adapter.
//!
//! Consumes the shared `origin_provider::ndjson::from_reqwest` line stream and
//! emits canonical token events into a `Ring`. Iteration stops after the first
//! frame with `done = true`.

use crate::wire::WireFrame;
use futures_util::StreamExt;
use origin_provider::ndjson;
use origin_provider::ProviderError;
use origin_stream::{Ring, TokenEvent, TokenKind};

/// Drive the Ollama NDJSON stream from `resp` and publish into `ring`.
///
/// Does not call `ring.close()` — the caller controls ring lifecycle.
///
/// # Errors
/// Returns `ProviderError::Api` on JSON or ring failures and propagates
/// `ProviderError` from the NDJSON pump.
pub async fn parse_into_ring(resp: reqwest::Response, ring: &Ring) -> Result<(), ProviderError> {
    let stream = ndjson::from_reqwest(resp);
    pin_utils::pin_mut!(stream);

    while let Some(item) = stream.next().await {
        let raw = item?;
        if raw.is_empty() {
            continue;
        }

        let frame: WireFrame = match serde_json::from_str(&raw) {
            Ok(f) => f,
            Err(e) => return Err(ProviderError::Api(format!("ndjson json: {e}; raw={raw}"))),
        };

        if !frame.message.content.is_empty() {
            ring.publish(&TokenEvent::new(
                TokenKind::TextDelta,
                frame.message.content.into_bytes(),
            ))
            .map_err(|e| ProviderError::Api(e.to_string()))?;
        }

        if frame.done {
            // Publish Usage BEFORE TurnEnd: the daemon's drain loop terminates
            // on TurnEnd, so a Usage emitted afterwards is dropped and the
            // turn's token counts are lost.
            let payload = encode_usage(frame.prompt_eval_count, frame.eval_count);
            ring.publish(&TokenEvent::new(TokenKind::Usage, payload))
                .map_err(|e| ProviderError::Api(e.to_string()))?;
            ring.publish(&TokenEvent::new(TokenKind::TurnEnd, Vec::new()))
                .map_err(|e| ProviderError::Api(e.to_string()))?;
            break;
        }
    }
    Ok(())
}

/// Encode prompt/eval counts as 4 × u32 BE: `[prompt, eval, 0, 0]`.
fn encode_usage(prompt_eval: u32, eval: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&prompt_eval.to_be_bytes());
    out.extend_from_slice(&eval.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out
}
