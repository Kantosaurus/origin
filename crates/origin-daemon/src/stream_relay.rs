//! Bridge a per-request `Ring` to an `origin-ipc` connection as JSON-encoded
//! `Event` frames.

use crate::protocol::StreamEvent;
use origin_ipc::frame::FrameKind;
use origin_ipc::transport::SharedConnection;
use origin_stream::{RingError, Subscriber, TokenKind};
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
    while let Some(tev) = sub.next().await? {
        let sev = match tev.kind() {
            TokenKind::TextDelta => StreamEvent::TextDelta {
                text: String::from_utf8_lossy(tev.payload()).into_owned(),
            },
            TokenKind::ToolUseDelta => StreamEvent::ToolUseDelta {
                partial_json: String::from_utf8_lossy(tev.payload()).into_owned(),
            },
            TokenKind::ThinkingDelta => StreamEvent::ThinkingDelta {
                thinking: String::from_utf8_lossy(tev.payload()).into_owned(),
            },
            TokenKind::Usage => {
                let p = tev.payload();
                if p.len() != 16 {
                    continue;
                }
                StreamEvent::Usage {
                    input_tokens: u32::from_be_bytes(p[0..4].try_into().expect("4 bytes")),
                    output_tokens: u32::from_be_bytes(p[4..8].try_into().expect("4 bytes")),
                    cache_read_input_tokens: u32::from_be_bytes(p[8..12].try_into().expect("4 bytes")),
                    cache_creation_input_tokens: u32::from_be_bytes(p[12..16].try_into().expect("4 bytes")),
                }
            }
            TokenKind::TurnEnd => StreamEvent::TurnEnd,
        };
        let body = serde_json::to_vec(&sev)?;
        conn.lock().await.write_frame(FrameKind::Event, &body).await?;
    }
    Ok(())
}
