// SPDX-License-Identifier: Apache-2.0
//! Regression tests for two bugs flagged in the P2.10 code-quality review:
//!
//! - Bug 1: `agent::run_loop` hangs forever when `provider.chat_stream` returns
//!   `Err` BEFORE calling `ring.close()` (drain awaits a never-closing ring).
//! - Bug 2: the per-connection relay (Event frames) and the handler (Response
//!   frame) race on the shared connection — `Response` can be written while
//!   the relay still has buffered `TextDelta` / `TurnEnd` events.

#![allow(clippy::panic)]

use async_trait::async_trait;
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::protocol::StreamEvent;
use origin_daemon::session::Session;
use origin_daemon::stream_relay::relay_to_connection;
use origin_ipc::frame::FrameKind;
use origin_ipc::transport::{Connector, Listener};
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError};
use origin_stream::{Ring, Subscriber, TokenEvent, TokenKind};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

// ---------- Bug 1 regression ---------------------------------------------

/// A streaming provider that immediately returns `Err` from `chat_stream`
/// without ever publishing on, or closing, the ring. Mirrors the real bug:
/// upstream API blew up before the trailing `ring.close()`.
struct StreamFailsImmediately;

#[async_trait]
impl Provider for StreamFailsImmediately {
    fn name(&self) -> &'static str {
        "stream-fails-immediately"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        Err(ProviderError::Api("non-stream path should not be hit".into()))
    }
    async fn chat_stream(&self, _req: ChatRequest, _ring: &Ring) -> Result<(), ProviderError> {
        // Deliberately do NOT touch the ring. Return Err so the agent's
        // streaming branch must terminate without hanging on the drain.
        Err(ProviderError::Api("simulated upstream failure".into()))
    }
}

#[tokio::test]
async fn run_loop_terminates_when_chat_stream_errors_before_closing_ring() {
    let provider = StreamFailsImmediately;
    let mut session = Session::new("test", "claude-opus-4-7");
    // Hard deadline: if the bug is unfixed, the drain hangs forever and the
    // outer `timeout` will fire. With the fix the loop returns within ms.
    let res = tokio::time::timeout(
        Duration::from_millis(500),
        run_loop(
            &mut session,
            "hello",
            &provider,
            &AlwaysAllow,
            &LoopOptions::default(),
        ),
    )
    .await
    .expect("run_loop must terminate within 500ms even on chat_stream error");

    // Should propagate the provider error as a LoopError::Provider.
    let err = res.expect_err("must surface the provider error");
    let msg = format!("{err}");
    assert!(msg.contains("simulated upstream failure"), "got: {msg}");
}

// ---------- Bug 2 regression ---------------------------------------------

/// A scripted streaming provider that emits a configurable number of
/// `TextDelta`s plus a `TurnEnd` over the ring. We deliberately publish
/// quickly and then close — the relay should still flush every event to the
/// client BEFORE the handler-equivalent writes the `Response` frame.
struct ScriptedStreamProvider {
    deltas: Vec<&'static str>,
}

#[async_trait]
impl Provider for ScriptedStreamProvider {
    fn name(&self) -> &'static str {
        "scripted-stream"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        Err(ProviderError::Api("non-stream path should not be hit".into()))
    }
    async fn chat_stream(&self, _req: ChatRequest, ring: &Ring) -> Result<(), ProviderError> {
        for d in &self.deltas {
            ring.publish(&TokenEvent::new(TokenKind::TextDelta, d.as_bytes().to_vec()))
                .map_err(|e| ProviderError::Api(e.to_string()))?;
        }
        ring.publish(&TokenEvent::new(TokenKind::TurnEnd, Vec::new()))
            .map_err(|e| ProviderError::Api(e.to_string()))?;
        ring.close();
        Ok(())
    }
}

/// End-to-end ordering test that mimics `handle_request` precisely:
/// - Spawns a per-request relay task wired to the same `SharedConnection`.
/// - Runs `run_loop` with `relay_tx` so the agent's per-turn `Subscriber` is
///   sent to the relay.
/// - After `run_loop` returns, drops `tx_sub` and awaits the relay's
///   `JoinHandle` BEFORE writing the `Response` frame.
///
/// The client reads every frame and asserts that the full set of `TextDelta`s
/// (plus `TurnEnd`) is observed BEFORE the final `Response` frame.
#[tokio::test]
async fn relay_drains_all_events_before_response_frame() {
    let path = unique_path("ordering");
    let listener = Listener::bind(&path).await.expect("bind");
    let path_clone = path.clone();

    let server = tokio::spawn(async move {
        let conn = listener.accept().await.expect("accept");
        let shared = Arc::new(Mutex::new(conn));

        let (tx_sub, mut rx_sub) = mpsc::channel::<Subscriber>(1);
        let conn_for_relay = Arc::clone(&shared);
        let relay_handle = tokio::spawn(async move {
            while let Some(sub) = rx_sub.recv().await {
                relay_to_connection(sub, Arc::clone(&conn_for_relay))
                    .await
                    .expect("relay");
            }
        });

        // 100 small deltas to make it likely that the relay still has writes
        // buffered in the conn mutex queue at the time `run_loop` returns.
        let deltas: Vec<&'static str> = (0..100).map(|_| "x").collect();
        let provider = ScriptedStreamProvider { deltas };
        let mut session = Session::new("test", "claude-opus-4-7");
        // Scope opts so its `relay_tx` Sender clone is dropped before we
        // await the relay handle — otherwise the channel never closes.
        let summary = {
            let opts = LoopOptions {
                max_turns: 1,
                cas: None,
                relay_tx: Some(tx_sub.clone()),
                streaming_disabled: false,
                ..LoopOptions::default()
            };
            run_loop(&mut session, "hi", &provider, &AlwaysAllow, &opts)
                .await
                .expect("run_loop")
        };
        drop(tx_sub);
        relay_handle.await.expect("relay join");

        // NOW write the Response frame.
        let reply = origin_daemon::protocol::PromptReply {
            assistant_text: summary.assistant_text,
            turns: summary.turns,
        };
        let bytes = serde_json::to_vec(&reply).expect("encode reply");
        shared
            .lock()
            .await
            .write_frame(FrameKind::Response, &bytes)
            .await
            .expect("write response");
    });

    let mut client = Connector::connect(&path_clone).await.expect("connect");
    let mut total_text = String::new();
    let mut saw_turn_end = false;
    let mut saw_response = false;
    let mut response_before_turn_end = false;
    let mut text_after_response = false;

    // Read frames until we see the Response. We use peek_frame_kind via the
    // header bytes by reading frames in a loop and inspecting type via the
    // raw read_frame call below.
    loop {
        let (kind, body) = client.read_frame().await.expect("read frame");
        match kind {
            FrameKind::Event => {
                if saw_response {
                    text_after_response = true;
                }
                let ev: StreamEvent = serde_json::from_slice(&body).expect("decode event");
                match ev {
                    StreamEvent::TextDelta { text } => total_text.push_str(&text),
                    StreamEvent::TurnEnd => {
                        if saw_response {
                            response_before_turn_end = true;
                        }
                        saw_turn_end = true;
                    }
                    _ => {}
                }
            }
            FrameKind::Response => {
                saw_response = true;
                break;
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }

    assert!(saw_response, "must receive a Response frame");
    // The relay coalesces consecutive ready TextDeltas into fewer frames, so we
    // assert on the total delivered *content* (no text lost) rather than the
    // frame count, which is now an implementation detail.
    assert_eq!(
        total_text,
        "x".repeat(100),
        "all TextDelta content must arrive before Response"
    );
    assert!(saw_turn_end, "TurnEnd must arrive before Response");
    assert!(
        !response_before_turn_end,
        "Response was written before TurnEnd Event frame"
    );
    assert!(
        !text_after_response,
        "Event frame arrived after Response — relay was not drained before reply"
    );

    server.await.expect("server");
}

fn unique_path(label: &str) -> String {
    let pid = std::process::id();
    let nano = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\origin-test-{label}-{pid}-{nano}")
    }
    #[cfg(unix)]
    {
        format!(
            "{}/origin-test-{label}-{pid}-{nano}.sock",
            std::env::temp_dir().display()
        )
    }
}
