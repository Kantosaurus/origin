// SPDX-License-Identifier: Apache-2.0
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_openai::OpenAi;
use origin_stream::{Ring, TokenKind};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn build_sse_body() -> String {
    let frames = [
        r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"hel"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":"lo"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":" w"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":"orld"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
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

#[tokio::test]
async fn openai_streams_text_then_turn_end() {
    let server = MockServer::start().await;
    let sse_body = build_sse_body();

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-test"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body.into_bytes(), "text/event-stream"))
        .mount(&server)
        .await;

    let prov = OpenAi::with_base_url("sk-test", &server.uri());
    let ring = Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();

    let r = ring.clone();
    let prov_handle = tokio::spawn(async move {
        prov.chat_stream(
            ChatRequest {
                system: String::new(),
                messages: vec![Message::new(Role::User).with_block(Block::text("hi"))],
                model: "gpt-4o".into(),
                tools: vec![],
                effort: None,
                thinking_tokens: None,
                attachments: Vec::new(),
            },
            &r,
        )
        .await
        .expect("stream");
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
    assert_eq!(text, "hello world");
    assert!(saw_turn_end, "expected TurnEnd");
}
