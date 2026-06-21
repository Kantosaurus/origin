// SPDX-License-Identifier: Apache-2.0
//! A single undecodable SSE data frame must NOT abort the whole streaming turn.
//!
//! Regression test for accuracy fix C18: previously the openai-compat SSE pump
//! returned `Err` on the first frame whose JSON failed to parse, dropping every
//! block already streamed. The Anthropic provider skips the bad frame and keeps
//! going; this proves the openai-compat path now mirrors that — both good frames
//! survive a garbage frame wedged between them, and the turn still completes.

#![allow(clippy::unwrap_used, clippy::panic)]

use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_openai_compat::{OpenAiCompat, OpenAiCompatConfig, StaticBearer};
use origin_stream::{Ring, Subscriber, TokenKind};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// "Hi" + a garbage (non-JSON) frame + "!" + a clean `finish_reason` frame.
fn build_sse_body_with_garbage() -> String {
    let frames = [
        r#"{"choices":[{"delta":{"content":"Hi"}}]}"#,
        // Garbage: truncated / non-JSON data frame between two good ones.
        r#"{"choices":[{"delta":{"content":"#,
        r#"{"choices":[{"delta":{"content":"!"}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
    ];
    let mut body = String::new();
    for f in frames {
        body.push_str("data: ");
        body.push_str(f);
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

fn make_req() -> ChatRequest {
    ChatRequest {
        system: String::new(),
        messages: vec![Message::new(Role::User).with_block(Block::text("hi"))],
        model: "gpt-test".into(),
        tools: vec![],
        effort: None,
        thinking_tokens: None,
        attachments: Vec::new(),
    }
}

fn config(base_url: String) -> OpenAiCompatConfig {
    OpenAiCompatConfig {
        name: "test",
        base_url,
        chat_path: "/v1/chat/completions".to_string(),
        auth: StaticBearer::new("sk-test"),
        extra_headers: vec![],
    }
}

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
async fn garbage_frame_between_good_frames_is_skipped_not_fatal() {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(build_sse_body_with_garbage().into_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let provider = OpenAiCompat::new(config(base));
    let ring = Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();
    let r = ring.clone();
    let prov = tokio::spawn(async move {
        // The turn must complete successfully despite the garbage frame.
        provider.chat_stream(make_req(), &r).await.expect("stream must not abort on bad frame");
    });
    let (text, saw_turn_end) = drain(&mut sub).await;
    prov.await.expect("prov task");

    assert_eq!(text, "Hi!", "both good frames' content must survive a skipped garbage frame");
    assert!(saw_turn_end, "the turn must still reach TurnEnd after a skipped frame");
}
