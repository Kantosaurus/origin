// SPDX-License-Identifier: Apache-2.0
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_openrouter::OpenRouter;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
#[allow(clippy::panic)]
async fn calls_openrouter_chat_completions_endpoint() {
    let server = MockServer::start().await;

    let response_body = json!({
        "id": "gen-1",
        "object": "chat.completion",
        "model": "openrouter/auto",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "ok"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1
        }
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(header("authorization", "Bearer test-key"))
        .and(header("http-referer", "https://origin.local"))
        .and(header("x-title", "origin"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
        .mount(&server)
        .await;

    let provider = OpenRouter::with_base_url("test-key", &server.uri());
    let req = ChatRequest {
        system: String::new(),
        messages: vec![Message::new(Role::User).with_block(Block::text("ping"))],
        model: "openrouter/auto".into(),
        tools: vec![],
    };
    let resp = provider.chat(req).await.expect("openrouter chat should succeed");

    assert_eq!(resp.assistant.role, Role::Assistant);
    assert_eq!(resp.usage.input_tokens, 1);
    assert_eq!(resp.usage.output_tokens, 1);
    assert_eq!(resp.assistant.blocks.len(), 1);
    match &resp.assistant.blocks[0] {
        Block::Text { text, .. } => assert_eq!(text, "ok"),
        other => panic!("expected Text block, got {other:?}"),
    }
}

#[tokio::test]
async fn auth_error_maps_to_provider_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "code": 401, "message": "no auth" }
        })))
        .mount(&server)
        .await;
    let provider = OpenRouter::with_base_url("bad", &server.uri());
    let err = provider
        .chat(ChatRequest {
            system: String::new(),
            messages: vec![],
            model: "openrouter/auto".into(),
            tools: vec![],
        })
        .await
        .expect_err("should fail with auth");
    assert!(matches!(err, origin_provider::ProviderError::Auth));
}
