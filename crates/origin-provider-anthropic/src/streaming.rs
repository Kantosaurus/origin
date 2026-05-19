//! SSE → `TokenEvent` adapter.
//!
//! Reads an Anthropic `text/event-stream` body byte stream and writes
//! `TokenEvent`s into an `origin_stream::Ring`. Handles `content_block_delta`,
//! `message_delta` (`stop_reason` → `TurnEnd`), and `message_delta` usage.

use futures_util::StreamExt;
use origin_stream::{Ring, RingError, TokenEvent, TokenKind};
use serde::Deserialize;
use thiserror::Error;
use tokio_util::io::ReaderStream;

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum StreamingError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("ring: {0}")]
    Ring(#[from] RingError),
    #[error("sse: {0}")]
    Sse(String),
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireEvent {
    MessageStart {},
    ContentBlockStart {},
    ContentBlockDelta {
        #[allow(dead_code)]
        index: u32,
        delta: WireDelta,
    },
    ContentBlockStop {},
    MessageDelta {
        #[serde(default)]
        delta: Option<WireMessageDelta>,
        #[serde(default)]
        usage: Option<WireUsage>,
    },
    MessageStop {},
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct WireMessageDelta {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[allow(clippy::struct_field_names)]
#[derive(Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
}

/// Parse an SSE byte stream into the ring. Works with both `&[u8]` slices
/// (tests) and live reqwest byte streams (production path).
///
/// # Errors
/// Propagates I/O or ring errors. Malformed JSON inside an `event` is reported
/// as `Sse(String)`.
pub async fn parse_into_ring<R>(reader: R, ring: &Ring) -> Result<(), StreamingError>
where
    R: tokio::io::AsyncRead + Unpin + Send,
{
    use eventsource_stream::Eventsource;
    let stream = ReaderStream::new(reader).eventsource();
    pin_utils::pin_mut!(stream);

    while let Some(item) = stream.next().await {
        let ev = item.map_err(|e| StreamingError::Sse(e.to_string()))?;
        let raw = ev.data;
        if raw.is_empty() {
            continue;
        }
        let parsed: WireEvent =
            serde_json::from_str(&raw).map_err(|e| StreamingError::Sse(format!("json: {e}; raw={raw}")))?;
        match parsed {
            WireEvent::ContentBlockDelta {
                delta: WireDelta::TextDelta { text },
                ..
            } => {
                ring.publish(&TokenEvent::new(TokenKind::TextDelta, text.into_bytes()))?;
            }
            WireEvent::ContentBlockDelta {
                delta: WireDelta::InputJsonDelta { partial_json },
                ..
            } => {
                ring.publish(&TokenEvent::new(
                    TokenKind::ToolUseDelta,
                    partial_json.into_bytes(),
                ))?;
            }
            WireEvent::ContentBlockDelta {
                delta: WireDelta::ThinkingDelta { thinking },
                ..
            } => {
                ring.publish(&TokenEvent::new(TokenKind::ThinkingDelta, thinking.into_bytes()))?;
            }
            WireEvent::MessageDelta { delta, usage } => {
                if let Some(d) = delta {
                    if d.stop_reason.is_some() {
                        ring.publish(&TokenEvent::new(TokenKind::TurnEnd, Vec::new()))?;
                    }
                }
                if let Some(u) = usage {
                    let payload = encode_usage(&u);
                    ring.publish(&TokenEvent::new(TokenKind::Usage, payload))?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn encode_usage(u: &WireUsage) -> Vec<u8> {
    // 4 × u32 BE. Order: input, output, cache_read, cache_creation.
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&u.input_tokens.to_be_bytes());
    out.extend_from_slice(&u.output_tokens.to_be_bytes());
    out.extend_from_slice(&u.cache_read_input_tokens.to_be_bytes());
    out.extend_from_slice(&u.cache_creation_input_tokens.to_be_bytes());
    out
}
