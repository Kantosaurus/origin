// SPDX-License-Identifier: Apache-2.0
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_gemini::Gemini;
use serde_json::json;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
#[allow(clippy::panic)]
async fn calls_gemini_generate_content_endpoint() {
    let server = MockServer::start().await;

    let response_body = json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [
                    {"text": "hello"},
                    {"functionCall": {"name": "fs_read", "args": {"path": "x"}}}
                ]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 5,
            "candidatesTokenCount": 2,
            "cachedContentTokenCount": 1
        }
    });

    Mock::given(method("POST"))
        .and(path_regex(r"/v1beta/models/.*:generateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
        .mount(&server)
        .await;

    let provider = Gemini::with_base_url("k-test", &server.uri());
    let req = ChatRequest {
        system: "be terse".into(),
        messages: vec![Message::new(Role::User).with_block(Block::text("ping"))],
        model: "gemini-2.0-flash".into(),
        tools: vec![],
    };
    let resp = provider.chat(req).await.expect("gemini chat should succeed");

    assert_eq!(resp.assistant.role, Role::Assistant);
    assert_eq!(resp.usage.input_tokens, 5);
    assert_eq!(resp.usage.output_tokens, 2);
    assert_eq!(resp.usage.cache_read_input_tokens, 1);

    let mut saw_text = false;
    let mut saw_tool_use = false;
    for b in &resp.assistant.blocks {
        match b {
            Block::Text { text, .. } if text == "hello" => saw_text = true,
            Block::ToolUse {
                id, name, input_json, ..
            } if id == "call_fs_read" && name == "fs_read" => {
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
        .and(path_regex(r"/v1beta/models/.*:generateContent"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "code": 401, "message": "bad key" }
        })))
        .mount(&server)
        .await;
    let provider = Gemini::with_base_url("bad", &server.uri());
    let err = provider
        .chat(ChatRequest {
            system: String::new(),
            messages: vec![],
            model: "gemini-2.0-flash".into(),
            tools: vec![],
        })
        .await
        .expect_err("should fail with auth");
    assert!(matches!(err, origin_provider::ProviderError::Auth));
}
