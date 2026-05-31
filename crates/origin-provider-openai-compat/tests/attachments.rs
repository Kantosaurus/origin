// SPDX-License-Identifier: Apache-2.0
//! Verifies multimodal attachments are injected into the `OpenAI`-compat request
//! body (item G-live). The mock only responds when the last user message's
//! content carries an `image_url` part, so a passing `chat()` proves the
//! attachment was encoded and injected end-to-end.

use origin_core::types::{Block, Message, Role};
use origin_multimodal::ContentBlock;
use origin_provider::{ChatRequest, Provider};
use origin_provider_openai_compat::{OpenAiCompat, OpenAiCompatConfig, StaticBearer};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn req_with(attachments: Vec<ContentBlock>) -> ChatRequest {
    ChatRequest {
        system: String::new(),
        messages: vec![Message {
            role: Role::User,
            blocks: vec![Block::Text {
                text: "describe this".to_string(),
                cache_marker: None,
            }],
        }],
        model: "gpt-x".to_string(),
        tools: Vec::new(),
        effort: None,
        thinking_tokens: None,
        attachments,
    }
}

/// `true` when the last user message's content array contains an `image_url` part.
fn body_has_image_url(req: &Request) -> bool {
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(&req.body) else {
        return false;
    };
    let Some(messages) = body.get("messages").and_then(serde_json::Value::as_array) else {
        return false;
    };
    messages.iter().rev().any(|m| {
        m.get("role").and_then(serde_json::Value::as_str) == Some("user")
            && m.get("content")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|parts| {
                    parts.iter().any(|p| {
                        p.get("type").and_then(serde_json::Value::as_str) == Some("image_url")
                    })
                })
    })
}

fn provider(uri: &str) -> OpenAiCompat {
    OpenAiCompat::new(OpenAiCompatConfig {
        name: "test",
        base_url: uri.to_string(),
        chat_path: "/v1/chat/completions".to_string(),
        auth: StaticBearer::new("sk-test"),
        extra_headers: vec![],
    })
}

#[tokio::test]
async fn image_attachment_becomes_image_url_part() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_has_image_url)
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": "a red dot" } }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
        })))
        .mount(&server)
        .await;

    let png = ContentBlock::image("image/png", "aGVsbG8=");
    let resp = provider(&server.uri())
        .chat(req_with(vec![png]))
        .await
        .expect("chat with image attachment must match the image_url-requiring mock");
    assert!(!resp.assistant.blocks.is_empty());
}

#[tokio::test]
async fn image_attachment_becomes_image_url_part_on_streaming_path() {
    // Regression: `chat_stream` (the DEFAULT path) previously skipped
    // `append_attachments`, silently dropping images on every streamed turn.
    // The mock only matches when the streaming body carries an `image_url`
    // part, so a successful stream proves injection on the streaming path.
    let server = MockServer::start().await;
    let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_has_image_url)
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse.as_bytes().to_vec(), "text/event-stream"))
        .mount(&server)
        .await;

    let png = ContentBlock::image("image/png", "aGVsbG8=");
    let prov = provider(&server.uri());
    let ring = origin_stream::Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();
    let r = ring.clone();
    let handle = tokio::spawn(async move {
        prov.chat_stream(req_with(vec![png]), &r)
            .await
            .expect("streaming chat with an image must match the image_url-requiring mock");
    });
    while sub.next().await.expect("recv").is_some() {}
    handle.await.expect("prov task");
}
