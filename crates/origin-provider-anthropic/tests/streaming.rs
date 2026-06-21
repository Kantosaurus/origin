// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::panic)]
use origin_provider_anthropic::streaming::{is_retryable_error_kind, parse_into_ring, StreamingError};
use origin_stream::{Ring, TokenKind};
use std::path::PathBuf;

#[tokio::test]
async fn fixture_emits_two_text_deltas_then_turn_end() {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests/fixtures/sse_hello.txt");
    let body = std::fs::read(&path).expect("read fixture");

    let ring = Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();

    let r = ring.clone();
    let parser = tokio::spawn(async move {
        parse_into_ring(body.as_slice(), &r).await.expect("parse");
        r.close();
    });

    let mut text_payloads: Vec<String> = Vec::new();
    let mut saw_turn_end = false;
    let mut saw_usage = false;
    while let Some(ev) = sub.next().await.expect("recv") {
        match ev.kind() {
            TokenKind::TextDelta => text_payloads.push(String::from_utf8_lossy(ev.payload()).into_owned()),
            TokenKind::TurnEnd => saw_turn_end = true,
            TokenKind::Usage => saw_usage = true,
            _ => {}
        }
    }
    parser.await.expect("parser");

    assert_eq!(text_payloads, vec!["Hel".to_string(), "lo!".to_string()]);
    assert!(saw_turn_end, "expected TurnEnd");
    assert!(saw_usage, "expected Usage");
}

#[tokio::test]
async fn mid_stream_error_event_surfaces_as_retryable() {
    // A `{"type":"error","error":{"type":"overloaded_error",...}}` event must be
    // surfaced (not swallowed by serde(other)) so the provider can retry it.
    let sse =
        b"data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
    let ring = Ring::with_capacity(64 * 1024);
    let err = parse_into_ring(sse.as_slice(), &ring)
        .await
        .expect_err("mid-stream error must surface");
    match err {
        StreamingError::MidStream { kind, .. } => {
            assert_eq!(kind, "overloaded_error");
            assert!(is_retryable_error_kind(&kind), "overloaded_error is retryable");
        }
        other => panic!("expected MidStream, got {other:?}"),
    }
}

#[tokio::test]
async fn single_garbage_frame_does_not_abort_the_turn() {
    // One undecodable frame, then a valid text delta + stop. The garbage frame
    // must be skipped (not abort), and the valid content + TurnEnd still arrive.
    let sse = b"data: {not valid json}\n\n\
                data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n\
                data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n";
    let ring = Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();
    let r = ring.clone();
    let parser = tokio::spawn(async move {
        parse_into_ring(sse.as_slice(), &r)
            .await
            .expect("a single garbage frame must not abort the turn");
        r.close();
    });
    let mut text = String::new();
    let mut saw_turn_end = false;
    while let Some(ev) = sub.next().await.expect("recv") {
        match ev.kind() {
            TokenKind::TextDelta => text.push_str(&String::from_utf8_lossy(ev.payload())),
            TokenKind::TurnEnd => saw_turn_end = true,
            _ => {}
        }
    }
    parser.await.expect("parser");
    assert_eq!(text, "hi", "valid content after the garbage frame survives");
    assert!(saw_turn_end, "TurnEnd still delivered");
}
