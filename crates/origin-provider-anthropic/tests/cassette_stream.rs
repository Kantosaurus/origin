// SPDX-License-Identifier: Apache-2.0
//! Record→replay cassette test for the Anthropic STREAMING path.
//!
//! Mirrors the non-streaming cassette test (`tests/cassette.rs`) but exercises
//! `chat_stream`: records a (mocked) SSE response to a cassette file, then
//! replays it from disk with NO server running, proving the `ORIGIN_CASSETTE`
//! tap both captures and serves the raw SSE event-stream without a network call
//! on replay. Also asserts the recorded SSE is secret-scrubbed before it lands
//! on disk.

#![allow(clippy::unwrap_used, clippy::panic)]

use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_anthropic::Anthropic;
use origin_stream::{Ring, Subscriber, TokenKind};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// An Anthropic SSE body whose text deltas spell "Hi!" and which ends with a
/// `message_delta` carrying a `stop_reason` (→ `TurnEnd`). A secret-looking
/// token is embedded in a delta so we can prove the recorded cassette is
/// scrubbed before it touches disk.
fn build_sse_body() -> String {
    let frames = [
        r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"claude-test","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":7,"output_tokens":0}}}"#,
        r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"!"}}"#,
        // A live secret in the stream body: must be redacted in the cassette.
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"key sk-live-abcdef0123456789abcdef0123456789"}}"#,
        r#"event: content_block_stop
data: {"type":"content_block_stop","index":0}"#,
        r#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"input_tokens":7,"output_tokens":3}}"#,
        r#"event: message_stop
data: {"type":"message_stop"}"#,
    ];
    let mut body = String::new();
    for f in frames {
        body.push_str(f);
        body.push_str("\n\n");
    }
    body
}

fn make_req() -> ChatRequest {
    ChatRequest {
        system: String::new(),
        messages: vec![Message::new(Role::User).with_block(Block::text("hi"))],
        model: "claude-test".into(),
        tools: vec![],
        effort: None,
        thinking_tokens: None,
        attachments: Vec::new(),
    }
}

/// Drain a subscriber into (joined text, `saw_turn_end`). The subscriber must be
/// created BEFORE the producer is spawned, since `subscribe()` starts at the
/// current write cursor and a late subscriber would miss early records.
async fn drain(sub: &mut Subscriber) -> (String, bool) {
    let mut text = String::new();
    let mut saw_turn_end = false;
    while let Some(ev) = sub.next().await.expect("recv") {
        match ev.kind() {
            TokenKind::TextDelta => text.push_str(std::str::from_utf8(ev.payload()).expect("utf8")),
            TokenKind::TurnEnd => saw_turn_end = true,
            _ => {}
        }
    }
    (text, saw_turn_end)
}

#[tokio::test]
async fn records_then_replays_stream_without_network() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cassette_path = dir.path().join("anthropic-stream.json");
    let cassette_str = cassette_path.to_string_lossy().to_string();

    // --- Record phase: hit the mock server, capture the SSE to the cassette. ---
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(build_sse_body().into_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    // SAFETY: no other test in this binary reads ORIGIN_CASSETTE; the variable
    // is removed before the function returns so it cannot leak to siblings.
    std::env::set_var("ORIGIN_CASSETTE", format!("record:{cassette_str}"));
    let provider = Anthropic::with_base_url("k", &base);
    let ring = Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();
    let r = ring.clone();
    let prov = tokio::spawn(async move {
        provider.chat_stream(make_req(), &r).await.expect("record stream");
    });
    let (rec_text, rec_turn_end) = drain(&mut sub).await;
    prov.await.expect("record task");
    std::env::remove_var("ORIGIN_CASSETTE");

    assert!(rec_turn_end, "record path must still stream to the live caller");
    assert!(rec_text.starts_with("Hi!"), "record path text: {rec_text}");
    assert!(cassette_path.exists(), "cassette must be written on record");

    // The persisted SSE must be scrubbed: no live `sk-` token survives.
    let on_disk = std::fs::read_to_string(&cassette_path).expect("read cassette");
    assert!(
        !origin_cassette::contains_secret(&on_disk),
        "recorded SSE leaked a secret: {on_disk}"
    );
    assert!(on_disk.contains("***"), "expected redaction sentinel in cassette");

    // Drop the server so the socket is closed: a replay that touched the network
    // would now fail to connect. The cassette must serve the SSE from disk.
    drop(server);

    // --- Replay phase: server is gone; the recorded SSE is served from disk. ---
    std::env::set_var("ORIGIN_CASSETTE", format!("replay:{cassette_str}"));
    let provider = Anthropic::with_base_url("k", &base);
    let ring = Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();
    let r = ring.clone();
    let prov = tokio::spawn(async move {
        provider.chat_stream(make_req(), &r).await.expect("replay stream");
    });
    let (text, saw_turn_end) = drain(&mut sub).await;
    prov.await.expect("replay task");
    std::env::remove_var("ORIGIN_CASSETTE");

    assert!(saw_turn_end, "replay must surface TurnEnd from the recorded SSE");
    assert!(
        text.starts_with("Hi!"),
        "replay must reproduce the recorded TextDeltas (got: {text})"
    );
    // The scrubbed secret comes back as the redaction sentinel, never the raw key.
    assert!(!text.contains("sk-live"), "replayed text must not carry a live secret");
}
