// SPDX-License-Identifier: Apache-2.0
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_ollama::Ollama;
use origin_stream::{Ring, TokenKind};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn build_ndjson_body() -> Vec<u8> {
    let lines = [
        r#"{"message":{"role":"assistant","content":"hel"},"done":false}"#,
        r#"{"message":{"role":"assistant","content":"lo"},"done":false}"#,
        r#"{"message":{"role":"assistant","content":""},"done":true,"prompt_eval_count":4,"eval_count":2}"#,
    ];
    let mut body = String::new();
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    body.into_bytes()
}

#[tokio::test]
async fn ollama_streams_text_then_turn_end() {
    let server = MockServer::start().await;
    let body = build_ndjson_body();

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/x-ndjson"))
        .mount(&server)
        .await;

    let prov = Ollama::with_base_url(&server.uri());
    let ring = Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();

    let r = ring.clone();
    let prov_handle = tokio::spawn(async move {
        prov.chat_stream(
            ChatRequest {
                system: String::new(),
                messages: vec![Message::new(Role::User).with_block(Block::text("hi"))],
                model: "llama3".into(),
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
    let mut saw_usage = false;
    while let Some(ev) = sub.next().await.expect("recv") {
        match ev.kind() {
            TokenKind::TextDelta => {
                text.push_str(std::str::from_utf8(ev.payload()).expect("utf8"));
            }
            TokenKind::TurnEnd => saw_turn_end = true,
            TokenKind::Usage => {
                saw_usage = true;
                let p = ev.payload();
                assert_eq!(p.len(), 16);
                let prompt = u32::from_be_bytes([p[0], p[1], p[2], p[3]]);
                let eval = u32::from_be_bytes([p[4], p[5], p[6], p[7]]);
                assert_eq!(prompt, 4);
                assert_eq!(eval, 2);
            }
            _ => {}
        }
    }
    prov_handle.await.expect("prov task");
    assert_eq!(text, "hello");
    assert!(saw_turn_end, "expected TurnEnd");
    assert!(saw_usage, "expected Usage");
}

#[tokio::test]
async fn ollama_chat_non_streaming_round_trip() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "message": {"role": "assistant", "content": "hello"},
        "done": true,
        "prompt_eval_count": 7,
        "eval_count": 3
    });
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let prov = Ollama::with_base_url(&server.uri());
    let resp = prov
        .chat(ChatRequest {
            system: "be terse".into(),
            messages: vec![Message::new(Role::User).with_block(Block::text("ping"))],
            model: "llama3".into(),
            tools: vec![],
            effort: None,
            thinking_tokens: None,
            attachments: Vec::new(),
        })
        .await
        .expect("ollama chat should succeed");

    assert_eq!(resp.assistant.role, Role::Assistant);
    assert_eq!(resp.usage.input_tokens, 7);
    assert_eq!(resp.usage.output_tokens, 3);
    let saw_text = resp
        .assistant
        .blocks
        .iter()
        .any(|b| matches!(b, Block::Text { text, .. } if text == "hello"));
    assert!(saw_text, "expected text block");
}

#[tokio::test]
async fn ollama_provider_name_is_ollama() {
    let prov = Ollama::new();
    assert_eq!(prov.name(), "ollama");
}

#[tokio::test]
async fn ollama_auth_error_maps_to_provider_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let prov = Ollama::with_base_url(&server.uri());
    let err = prov
        .chat(ChatRequest {
            system: String::new(),
            messages: vec![],
            model: "llama3".into(),
            tools: vec![],
            effort: None,
            thinking_tokens: None,
            attachments: Vec::new(),
        })
        .await
        .expect_err("should fail with auth");
    assert!(matches!(err, origin_provider::ProviderError::Auth));
}
