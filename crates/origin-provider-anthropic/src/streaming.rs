// SPDX-License-Identifier: Apache-2.0
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
struct WireContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireEvent {
    MessageStart {
        #[serde(default)]
        message: Option<WireMessageStart>,
    },
    ContentBlockStart {
        index: u32,
        content_block: WireContentBlock,
    },
    ContentBlockDelta {
        index: u32,
        delta: WireDelta,
    },
    ContentBlockStop {
        #[serde(default)]
        index: Option<u32>,
    },
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
struct WireMessageStart {
    #[serde(default)]
    usage: Option<WireUsage>,
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
            WireEvent::ContentBlockStart {
                index,
                content_block: WireContentBlock { kind, id, name },
            } if kind == "tool_use" => {
                // Payload: 4-byte LE index + id bytes + '\0' + name bytes.
                let mut payload = index.to_le_bytes().to_vec();
                payload.extend_from_slice(id.as_bytes());
                payload.push(b'\0');
                payload.extend_from_slice(name.as_bytes());
                ring.publish(&TokenEvent::new(TokenKind::ToolUseStart, payload))?;
            }
            WireEvent::ContentBlockDelta {
                delta: WireDelta::TextDelta { text },
                ..
            } => {
                ring.publish(&TokenEvent::new(TokenKind::TextDelta, text.into_bytes()))?;
            }
            WireEvent::ContentBlockDelta {
                index,
                delta: WireDelta::InputJsonDelta { partial_json },
            } => {
                // Payload: 4-byte LE index + partial JSON bytes.
                let mut payload = index.to_le_bytes().to_vec();
                payload.extend_from_slice(partial_json.as_bytes());
                ring.publish(&TokenEvent::new(TokenKind::ToolUseDelta, payload))?;
            }
            WireEvent::ContentBlockDelta {
                delta: WireDelta::ThinkingDelta { thinking },
                ..
            } => {
                ring.publish(&TokenEvent::new(TokenKind::ThinkingDelta, thinking.into_bytes()))?;
            }
            WireEvent::MessageStart {
                message: Some(WireMessageStart { usage: Some(u) }),
            } => {
                let payload = encode_usage(&u);
                ring.publish(&TokenEvent::new(TokenKind::Usage, payload))?;
            }
            WireEvent::MessageDelta { delta, usage } => {
                // Publish the final Usage BEFORE TurnEnd: the daemon's drain loop
                // terminates on TurnEnd, so a Usage published afterwards would be
                // dropped and the turn's output/cache tokens lost.
                if let Some(u) = usage {
                    let payload = encode_usage(&u);
                    ring.publish(&TokenEvent::new(TokenKind::Usage, payload))?;
                }
                if let Some(d) = delta {
                    if d.stop_reason.is_some() {
                        ring.publish(&TokenEvent::new(TokenKind::TurnEnd, Vec::new()))?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Panic-free synchronous SSE wire-event decoder for fuzz targets.
///
/// Splits the input on SSE event boundaries (blank lines), strips a
/// leading `data:` prefix on each chunk, and feeds each payload through
/// the same `WireEvent` JSON decoder used by the live streaming path.
/// Any decode failure is reported via `ParseError`; this function MUST
/// NOT panic on arbitrary input.
///
/// # Errors
/// `ParseError::Empty` if the input contains no candidate JSON payloads.
/// `ParseError::Invalid` if a candidate payload fails to deserialize.
pub fn parse(bytes: &[u8]) -> Result<(), ParseError> {
    // Best-effort UTF-8 lift; reject up-front rather than panic on slicing.
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
        saw_any = true;
        if serde_json::from_str::<WireEvent>(&payload).is_err() {
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
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("no SSE payloads found")]
    Empty,
    #[error("invalid wire event")]
    Invalid,
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

/// Test-only view of a parsed SSE frame, exposing the per-block `index`
/// and any `usage` reported by Anthropic on `message_start` / `message_delta`.
///
/// This struct is intentionally minimal — the live `parse_into_ring` path
/// publishes into a `Ring` and does not surface this view. It exists so the
/// daemon and tests can verify that `index` is preserved end-to-end and so
/// future routing code can be wired on top.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TestFrame {
    pub index: Option<u32>,
    pub usage: Option<origin_provider::Usage>,
}

/// Parse a single SSE `data: …` line into a `TestFrame`. Returns `None` for
/// non-data lines, blank payloads, or JSON that does not match a recognised
/// `WireEvent` variant.
///
/// # Panics
/// Does not panic; all decode errors return `None`.
#[must_use]
pub fn parse_chunk_for_test(line: &[u8]) -> Option<TestFrame> {
    let text = std::str::from_utf8(line).ok()?;
    let payload = text
        .trim_start()
        .strip_prefix("data:")
        .map_or_else(|| text.trim(), str::trim_start)
        .trim();
    if payload.is_empty() || payload == "[DONE]" {
        return None;
    }
    let parsed: WireEvent = serde_json::from_str(payload).ok()?;
    let (index, wire_usage) = match parsed {
        WireEvent::ContentBlockStart { index, .. } | WireEvent::ContentBlockDelta { index, .. } => {
            (Some(index), None)
        }
        WireEvent::ContentBlockStop { index } => (index, None),
        WireEvent::MessageStart { message } => (None, message.and_then(|m| m.usage)),
        WireEvent::MessageDelta { usage, .. } => (None, usage),
        WireEvent::MessageStop {} | WireEvent::Other => (None, None),
    };
    Some(TestFrame {
        index,
        usage: wire_usage.map(|u| origin_provider::Usage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
            cache_creation_input_tokens: u.cache_creation_input_tokens,
        }),
    })
}
