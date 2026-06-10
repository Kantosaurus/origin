// SPDX-License-Identifier: Apache-2.0
//! Bridge a per-request `Ring` to an `origin-ipc` connection as JSON-encoded
//! `Event` frames.

use crate::protocol::StreamEvent;
use origin_ipc::frame::FrameKind;
use origin_ipc::transport::SharedConnection;
use origin_stream::{RingError, Subscriber, TokenEvent, TokenKind};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RelayError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("ring: {0}")]
    Ring(#[from] RingError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Drain a `Subscriber`, translate each `TokenEvent` into a JSON
/// `StreamEvent`, and write each as an `Event` frame on `conn`
/// (mutex-protected). Returns when the ring closes.
///
/// The caller is expected to create the `Subscriber` *before* the producer
/// (e.g. `provider.chat_stream`) begins publishing so no events are missed.
///
/// # Errors
/// Propagates I/O, ring decode, or JSON errors.
///
/// # Panics
/// Does not panic on well-formed input. Internal `expect`s operate on
/// fixed-size slices guaranteed to be present after explicit length checks.
pub async fn relay_to_connection(mut sub: Subscriber, conn: SharedConnection) -> Result<(), RelayError> {
    // Coalesce consecutive text deltas: each wake reads one event (blocking),
    // then drains every record already published (non-blocking) into the same
    // pass. A burst of tiny TextDeltas collapses into ONE frame/syscall instead
    // of one per delta — TTFT is preserved because the first delta still flushes
    // as soon as its pass completes. Non-text events flush any buffered text
    // first so ordering is unchanged.
    let mut pending_text = String::new();
    loop {
        let Some(first) = sub.next().await? else {
            flush_text(&mut pending_text, &conn).await?;
            break;
        };
        let mut current = Some(first);
        while let Some(tev) = current {
            if matches!(tev.kind(), TokenKind::TextDelta) {
                pending_text.push_str(&String::from_utf8_lossy(tev.payload()));
            } else {
                flush_text(&mut pending_text, &conn).await?;
                if let Some(sev) = translate(&tev) {
                    let body = serde_json::to_vec(&sev)?;
                    conn.lock().await.write_frame(FrameKind::Event, &body).await?;
                }
            }
            current = sub.try_next()?;
        }
        flush_text(&mut pending_text, &conn).await?;
    }
    Ok(())
}

/// Write the accumulated text (if any) as a single `TextDelta` frame, clearing
/// the buffer.
async fn flush_text(pending: &mut String, conn: &SharedConnection) -> Result<(), RelayError> {
    if pending.is_empty() {
        return Ok(());
    }
    let sev = StreamEvent::TextDelta {
        text: std::mem::take(pending),
    };
    let body = serde_json::to_vec(&sev)?;
    conn.lock().await.write_frame(FrameKind::Event, &body).await?;
    Ok(())
}

/// Translate a non-text `TokenEvent` into its `StreamEvent`. Returns `None` for
/// events the CLI relay does not surface (`ToolUseStart`, consumed by the agent
/// loop's speculative dispatch) or malformed `Usage` payloads. `TextDelta` is
/// handled by the coalescing path, not here.
fn translate(tev: &TokenEvent) -> Option<StreamEvent> {
    match tev.kind() {
        TokenKind::TextDelta => Some(StreamEvent::TextDelta {
            text: String::from_utf8_lossy(tev.payload()).into_owned(),
        }),
        TokenKind::ToolUseDelta => Some(StreamEvent::ToolUseDelta {
            partial_json: String::from_utf8_lossy(tev.payload()).into_owned(),
        }),
        TokenKind::ThinkingDelta => Some(StreamEvent::ThinkingDelta {
            thinking: String::from_utf8_lossy(tev.payload()).into_owned(),
        }),
        TokenKind::Usage => {
            let p = tev.payload();
            if p.len() != 16 {
                return None;
            }
            Some(StreamEvent::Usage {
                input_tokens: u32::from_be_bytes(p[0..4].try_into().expect("4 bytes")),
                output_tokens: u32::from_be_bytes(p[4..8].try_into().expect("4 bytes")),
                cache_read_input_tokens: u32::from_be_bytes(p[8..12].try_into().expect("4 bytes")),
                cache_creation_input_tokens: u32::from_be_bytes(p[12..16].try_into().expect("4 bytes")),
            })
        }
        TokenKind::TurnEnd => Some(StreamEvent::TurnEnd),
        TokenKind::ToolUseStart => None,
    }
}
