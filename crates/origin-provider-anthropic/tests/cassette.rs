// SPDX-License-Identifier: Apache-2.0
//! End-to-end cassette test for the Anthropic non-streaming `chat()` path.
//!
//! Records a real (mocked) provider exchange to a cassette file, then replays
//! it from disk with no server running, proving the `ORIGIN_CASSETTE` tap both
//! captures and serves provider responses without a network call on replay.

use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn make_req() -> ChatRequest {
    ChatRequest {
        system: String::new(),
        messages: vec![Message {
            role: Role::User,
            blocks: vec![Block::Text {
                text: "hi".to_string(),
                cache_marker: None,
            }],
        }],
        model: "claude-x".to_string(),
        tools: Vec::new(),
        effort: None,
        attachments: Vec::new(),
    }
}

#[tokio::test]
#[allow(clippy::panic)] // panic! in a test signals an assertion failure
async fn records_then_replays_without_network() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cassette_path = dir.path().join("anthropic.json");
    let cassette_str = cassette_path.to_string_lossy().to_string();

    // --- Record phase: hit the mock server, capture to the cassette file. ---
    let server = MockServer::start().await;
    let base = server.uri();
    let body = serde_json::json!({
        "content": [{"type": "text", "text": "hello back"}],
        "usage": {"input_tokens": 3, "output_tokens": 2}
    });
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    // SAFETY: no other test in this binary reads ORIGIN_CASSETTE; the variable
    // is removed before the function returns so it cannot leak to siblings.
    std::env::set_var("ORIGIN_CASSETTE", format!("record:{cassette_str}"));
    let provider = origin_provider_anthropic::Anthropic::with_base_url("k", &base);
    let resp = provider.chat(make_req()).await.expect("record chat");
    assert_eq!(resp.assistant.blocks.len(), 1);
    std::env::remove_var("ORIGIN_CASSETTE");

    assert!(cassette_path.exists(), "cassette file must be written on record");

    // Drop the server so the socket is closed: a replay that touched the network
    // would now fail to connect. The cassette must serve the response from disk.
    drop(server);

    // --- Replay phase: server is gone; the recorded body is served from disk. ---
    // The cassette keys on (method, url), so we reuse the recorded base URL; the
    // replay path short-circuits before any socket is opened.
    std::env::set_var("ORIGIN_CASSETTE", format!("replay:{cassette_str}"));
    let provider = origin_provider_anthropic::Anthropic::with_base_url("k", &base);
    let resp = provider.chat(make_req()).await.expect("replay chat");
    std::env::remove_var("ORIGIN_CASSETTE");

    assert_eq!(resp.assistant.blocks.len(), 1);
    match &resp.assistant.blocks[0] {
        Block::Text { text, .. } => assert_eq!(text, "hello back"),
        other => panic!("expected text block, got {other:?}"),
    }
}
