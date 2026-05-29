// SPDX-License-Identifier: Apache-2.0
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_anthropic::Anthropic;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
#[allow(clippy::panic)] // panic! in tests is intentional — signals assertion failure
async fn calls_anthropic_messages_endpoint() {
    let server = MockServer::start().await;

    let response_body = json!({
        "id": "msg_01",
        "type": "message",
        "role": "assistant",
        "model": "claude-opus-4-7",
        "content": [
            { "type": "text", "text": "hi" }
        ],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 10,
            "output_tokens": 5,
            "cache_read_input_tokens": 0,
            "cache_creation_input_tokens": 0
        }
    });

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
        .mount(&server)
        .await;

    let provider = Anthropic::with_base_url("test-key", &server.uri());
    let req = ChatRequest {
        system: "be terse".into(),
        messages: vec![Message::new(Role::User).with_block(Block::text("ping"))],
        model: "claude-opus-4-7".into(),
        tools: vec![],
    };
    let resp = provider.chat(req).await.expect("anthropic chat should succeed");

    assert_eq!(resp.assistant.role, Role::Assistant);
    let txt = match &resp.assistant.blocks[0] {
        Block::Text { text, .. } => text.clone(),
        other => panic!("expected text block, got {other:?}"),
    };
    assert_eq!(txt, "hi");
    assert_eq!(resp.usage.input_tokens, 10);
    assert_eq!(resp.usage.output_tokens, 5);
}

#[tokio::test]
async fn auth_error_maps_to_provider_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "type": "error",
            "error": { "type": "authentication_error", "message": "invalid key" }
        })))
        .mount(&server)
        .await;
    let provider = Anthropic::with_base_url("bad", &server.uri());
    let err = provider
        .chat(ChatRequest {
            system: String::new(),
            messages: vec![],
            model: "x".into(),
            tools: vec![],
        })
        .await
        .expect_err("should fail with auth");
    assert!(matches!(err, origin_provider::ProviderError::Auth));
}
