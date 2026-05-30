// SPDX-License-Identifier: Apache-2.0
//! Verifies multimodal attachments are injected into the Anthropic request body
//! (item G-live). The mock server only responds when the POST body actually
//! carries an image content block in the last user message, so a passing `chat()`
//! proves the attachment was encoded and injected end-to-end.

use origin_core::types::{Block, Message, Role};
use origin_multimodal::ContentBlock;
use origin_provider::{ChatRequest, Provider};
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
        model: "claude-x".to_string(),
        tools: Vec::new(),
        effort: None,
        attachments,
    }
}

/// `true` when the request body's last user message contains an `image` block.
fn body_has_image_block(req: &Request) -> bool {
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
                        p.get("type").and_then(serde_json::Value::as_str) == Some("image")
                    })
                })
    })
}

#[tokio::test]
async fn image_attachment_is_injected_into_last_user_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_has_image_block)
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "content": [{"type": "text", "text": "a red dot"}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .mount(&server)
        .await;

    let png = ContentBlock::image("image/png", "aGVsbG8=");
    let provider = origin_provider_anthropic::Anthropic::with_base_url("k", &server.uri());
    let resp = provider
        .chat(req_with(vec![png]))
        .await
        .expect("chat with image attachment must match the image-requiring mock");
    assert_eq!(resp.assistant.blocks.len(), 1);
}

#[test]
fn empty_attachments_leave_encoded_body_text_only() {
    let body = origin_provider_anthropic::encode_request_for_test(&req_with(Vec::new()));
    let content = &body["messages"][0]["content"];
    assert!(content.is_array(), "user content is a block array");
    assert_eq!(
        content.as_array().map(Vec::len),
        Some(1),
        "only the single text block; no attachment injected"
    );
}
