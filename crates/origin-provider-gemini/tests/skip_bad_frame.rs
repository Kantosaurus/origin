// SPDX-License-Identifier: Apache-2.0
//! A single undecodable SSE data frame must NOT abort the whole streaming turn.
//!
//! Regression test for accuracy fix C18: previously the gemini SSE pump returned
//! `Err` on the first frame whose JSON failed to parse, dropping every block
//! already streamed. The Anthropic provider skips the bad frame and keeps going;
//! this proves the gemini path now mirrors that — both good frames survive a
//! garbage frame wedged between them, and the turn still completes.

#![allow(clippy::unwrap_used, clippy::panic)]

use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_gemini::Gemini;
use origin_stream::{Ring, TokenKind};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// "hel" + a garbage (non-JSON) frame + "lo world" + a clean `finishReason` frame.
fn build_sse_body_with_garbage() -> String {
    let frames = [
        r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"hel"}]}}]}"#,
        // Garbage: truncated / non-JSON data frame between two good ones.
        r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"#,
        r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"lo world"}]}}]}"#,
        r#"{"candidates":[{"content":{"role":"model","parts":[{"text":""}]},"finishReason":"STOP"}]}"#,
    ];
    let mut body = String::new();
    for f in frames {
        body.push_str("data: ");
        body.push_str(f);
        body.push_str("\n\n");
    }
    body
}

#[tokio::test]
async fn garbage_frame_between_good_frames_is_skipped_not_fatal() {
    let server = MockServer::start().await;
    let sse_body = build_sse_body_with_garbage();

    Mock::given(method("POST"))
        .and(path_regex(r"/v1beta/models/.*:streamGenerateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body.into_bytes(), "text/event-stream"))
        .mount(&server)
        .await;

    let prov = Gemini::with_base_url("k-test", &server.uri());
    let ring = Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();

    let r = ring.clone();
    let prov_handle = tokio::spawn(async move {
        prov.chat_stream(
            ChatRequest {
                system: String::new(),
                messages: vec![Message::new(Role::User).with_block(Block::text("hi"))],
                model: "gemini-2.0-flash".into(),
                tools: vec![],
                effort: None,
                thinking_tokens: None,
                attachments: Vec::new(),
            },
            &r,
        )
        .await
        .expect("stream must not abort on bad frame");
    });

    let mut text = String::new();
    let mut saw_turn_end = false;
    while let Some(ev) = sub.next().await.expect("recv") {
        match ev.kind() {
            TokenKind::TextDelta => {
                text.push_str(std::str::from_utf8(ev.payload()).expect("utf8"));
            }
            TokenKind::TurnEnd => saw_turn_end = true,
            _ => {}
        }
    }
    prov_handle.await.expect("prov task");

    assert_eq!(text, "hello world", "both good frames' content must survive a skipped garbage frame");
    assert!(saw_turn_end, "the turn must still reach TurnEnd after a skipped frame");
}
