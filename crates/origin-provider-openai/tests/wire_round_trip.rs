use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_openai::OpenAi;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
#[allow(clippy::panic)]
async fn calls_openai_chat_completions_endpoint() {
    let server = MockServer::start().await;

    let response_body = json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "hello",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "fs_read",
                        "arguments": "{\"path\":\"x\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 4,
            "total_tokens": 14
        }
    });

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
        .mount(&server)
        .await;

    let provider = OpenAi::with_base_url("sk-test", &server.uri());
    let req = ChatRequest {
        system: "be terse".into(),
        messages: vec![Message::new(Role::User).with_block(Block::text("ping"))],
        model: "gpt-4o".into(),
        tools: vec![],
    };
    let resp = provider.chat(req).await.expect("openai chat should succeed");

    assert_eq!(resp.assistant.role, Role::Assistant);
    assert_eq!(resp.usage.input_tokens, 10);
    assert_eq!(resp.usage.output_tokens, 4);

    let mut saw_text = false;
    let mut saw_tool_use = false;
    for b in &resp.assistant.blocks {
        match b {
            Block::Text { text, .. } if text == "hello" => saw_text = true,
            Block::ToolUse {
                id, name, input_json, ..
            } if id == "call_1" && name == "fs_read" => {
                assert_eq!(input_json, br#"{"path":"x"}"#);
                saw_tool_use = true;
            }
            other => panic!("unexpected block {other:?}"),
        }
    }
    assert!(saw_text, "expected text block");
    assert!(saw_tool_use, "expected tool_use block");
}

#[tokio::test]
async fn auth_error_maps_to_provider_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "type": "invalid_api_key", "message": "bad key" }
        })))
        .mount(&server)
        .await;
    let provider = OpenAi::with_base_url("bad", &server.uri());
    let err = provider
        .chat(ChatRequest {
            system: String::new(),
            messages: vec![],
            model: "gpt-4o".into(),
            tools: vec![],
        })
        .await
        .expect_err("should fail with auth");
    assert!(matches!(err, origin_provider::ProviderError::Auth));
}
