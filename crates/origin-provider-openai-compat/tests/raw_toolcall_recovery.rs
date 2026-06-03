// SPDX-License-Identifier: Apache-2.0
//! End-to-end coverage that the non-streaming `Provider::chat` decode path
//! recovers a raw inline tool call from assistant text when a quirky backend
//! omits the structured `tool_calls` field. The mock server's localhost URI
//! classifies as `Backend::Other` (non-OpenAi), so the shimquirks fallback runs.

#![allow(clippy::unwrap_used, clippy::panic)]

use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_openai_compat::{OpenAiCompat, OpenAiCompatConfig, StaticBearer};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn user_req() -> ChatRequest {
    ChatRequest {
        system: String::new(),
        messages: vec![Message {
            role: Role::User,
            blocks: vec![Block::Text {
                text: "weather in NYC?".into(),
                cache_marker: None,
            }],
        }],
        model: "some-model".to_string(),
        tools: vec![],
        effort: None,
        thinking_tokens: None,
        attachments: Vec::new(),
    }
}

fn provider_for(server: &MockServer) -> OpenAiCompat {
    let cfg = OpenAiCompatConfig {
        name: "test",
        base_url: server.uri(),
        chat_path: "/v1/chat/completions".to_string(),
        auth: StaticBearer::new("sk-test"),
        extra_headers: vec![],
    };
    OpenAiCompat::new(cfg)
}

#[tokio::test]
async fn recovers_raw_text_tool_call_end_to_end() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "```json\n{\"name\": \"get_weather\", \"arguments\": {\"city\": \"NYC\"}}\n```"
                },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 4, "completion_tokens": 9 }
        })))
        .mount(&server)
        .await;

    let provider = provider_for(&server);
    let resp = provider.chat(user_req()).await.unwrap();

    assert_eq!(resp.assistant.blocks.len(), 1, "raw text replaced by tool use");
    match &resp.assistant.blocks[0] {
        Block::ToolUse { name, input_json, .. } => {
            assert_eq!(name, "get_weather");
            let args: serde_json::Value = serde_json::from_slice(input_json).unwrap();
            assert_eq!(args, json!({ "city": "NYC" }));
        }
        other => panic!("expected recovered ToolUse, got {other:?}"),
    }
}

#[tokio::test]
async fn structured_tool_call_path_unchanged() {
    // A backend that DOES return a structured tool_call must decode it normally;
    // the raw-text fallback stays inert.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_xyz",
                        "type": "function",
                        "function": { "name": "get_weather", "arguments": "{\"city\":\"NYC\"}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 4, "completion_tokens": 9 }
        })))
        .mount(&server)
        .await;

    let provider = provider_for(&server);
    let resp = provider.chat(user_req()).await.unwrap();

    assert_eq!(resp.assistant.blocks.len(), 1);
    match &resp.assistant.blocks[0] {
        Block::ToolUse { id, name, .. } => {
            assert_eq!(id, "call_xyz", "structured id preserved, not synthesised");
            assert_eq!(name, "get_weather");
        }
        other => panic!("expected structured ToolUse, got {other:?}"),
    }
}

#[tokio::test]
async fn plain_text_response_stays_text() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": { "role": "assistant", "content": "It is sunny in NYC." },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 4, "completion_tokens": 5 }
        })))
        .mount(&server)
        .await;

    let provider = provider_for(&server);
    let resp = provider.chat(user_req()).await.unwrap();

    assert_eq!(resp.assistant.blocks.len(), 1);
    match &resp.assistant.blocks[0] {
        Block::Text { text, .. } => assert_eq!(text, "It is sunny in NYC."),
        other => panic!("expected Text, got {other:?}"),
    }
}
