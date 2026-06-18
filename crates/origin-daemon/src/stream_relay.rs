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

/// Drain a `Subscriber`, forwarding the worker's assistant prose to its progress
/// channel.
///
/// Coalesces assistant `TextDelta` tokens and sends each run as
/// [`origin_swarm::WorkerProgress::AssistantText`] — the focused-agent
/// transcript's live prose. Mirrors [`relay_to_connection`]'s text coalescing
/// but targets the worker's progress channel instead of an IPC connection.
/// Non-text tokens are ignored here (a worker's tool activity reaches the panel
/// via its `event_tx` relay). Returns when the ring closes.
///
/// # Errors
/// Propagates ring decode errors.
pub async fn relay_to_progress(
    mut sub: Subscriber,
    progress: &origin_swarm::WorkerProgressTx,
) -> Result<(), RelayError> {
    let mut pending = String::new();
    loop {
        let Some(first) = sub.next().await? else {
            flush_text_to_progress(&mut pending, progress);
            break;
        };
        let mut current = Some(first);
        while let Some(tev) = current {
            if matches!(tev.kind(), TokenKind::TextDelta) {
                pending.push_str(&String::from_utf8_lossy(tev.payload()));
            } else {
                // A non-text token ends the current prose run; flush it so the
                // transcript ordering (text then tool) matches arrival order.
                flush_text_to_progress(&mut pending, progress);
            }
            current = sub.try_next()?;
        }
        flush_text_to_progress(&mut pending, progress);
    }
    Ok(())
}

/// Forward the accumulated text (if any) as one `AssistantText`, clearing the
/// buffer. A closed progress channel is ignored (the consumer simply stops).
fn flush_text_to_progress(pending: &mut String, progress: &origin_swarm::WorkerProgressTx) {
    if pending.is_empty() {
        return;
    }
    let _ = progress.send(origin_swarm::WorkerProgress::AssistantText(std::mem::take(pending)));
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

#[cfg(test)]
mod tests {
    use super::relay_to_progress;
    use origin_stream::{Ring, TokenEvent, TokenKind};

    #[tokio::test]
    async fn relay_to_progress_coalesces_text_into_assistant_runs() {
        // Subscribe BEFORE publishing so no records are missed.
        let ring = Ring::with_capacity(64 * 1024);
        let sub = ring.subscribe();
        let pub_ok = |ev| ring.publish(&ev).expect("publish token");
        pub_ok(TokenEvent::new(TokenKind::TextDelta, b"Hello ".to_vec()));
        pub_ok(TokenEvent::new(TokenKind::TextDelta, b"world".to_vec()));
        // A non-text token flushes the current prose run...
        pub_ok(TokenEvent::new(TokenKind::TurnEnd, Vec::new()));
        // ...and later text starts a fresh run.
        pub_ok(TokenEvent::new(TokenKind::TextDelta, b"more".to_vec()));
        ring.close();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        relay_to_progress(sub, &tx).await.expect("relay drains the ring");
        drop(tx);

        let mut texts = Vec::new();
        while let Ok(origin_swarm::WorkerProgress::AssistantText(t)) = rx.try_recv() {
            texts.push(t);
        }
        // Streamed deltas coalesce: "Hello world" as one run, "more" as a second.
        assert_eq!(texts, vec!["Hello world".to_string(), "more".to_string()]);
    }
}
