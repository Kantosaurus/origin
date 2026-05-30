// SPDX-License-Identifier: Apache-2.0
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_openai_compat::{OpenAiCompat, OpenAiCompatConfig, StaticBearer};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn happy_path_chat() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": { "role": "assistant", "content": "hello world" }
            }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 2 }
        })))
        .mount(&server)
        .await;

    let cfg = OpenAiCompatConfig {
        name: "test",
        base_url: server.uri(),
        chat_path: "/v1/chat/completions".to_string(),
        auth: StaticBearer::new("sk-test"),
        extra_headers: vec![],
    };
    let provider = OpenAiCompat::new(cfg);

    let req = ChatRequest {
        system: String::new(),
        messages: vec![Message {
            role: Role::User,
            blocks: vec![Block::Text {
                text: "hi".into(),
                cache_marker: None,
            }],
        }],
        model: "test-model".to_string(),
        tools: vec![],
        effort: None,
        attachments: Vec::new(),
    };

    let resp = provider.chat(req).await.unwrap();
    let text: String = resp
        .assistant
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "hello world");
    assert_eq!(resp.usage.input_tokens, 5);
    assert_eq!(resp.usage.output_tokens, 2);
}
