// SPDX-License-Identifier: Apache-2.0
//! Record→replay cassette test for the openai-compat NON-STREAMING `chat()` path.
//!
//! Mirrors the non-streaming Anthropic cassette test (`origin-provider-anthropic/
//! tests/cassette.rs`) and the openai-compat streaming cassette test, but
//! exercises `chat()`: records a (mocked) JSON response to a cassette file, then
//! replays it from disk with NO server running, proving the `ORIGIN_CASSETTE`
//! tap both captures and serves the OpenAI-shape response without a network call
//! on replay. Also asserts the recorded body is secret-scrubbed before it lands
//! on disk.

#![allow(clippy::unwrap_used, clippy::panic)]

use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_openai_compat::{OpenAiCompat, OpenAiCompatConfig, StaticBearer};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn make_req() -> ChatRequest {
    ChatRequest {
        system: String::new(),
        messages: vec![Message::new(Role::User).with_block(Block::text("hi"))],
        model: "gpt-test".into(),
        tools: vec![],
        effort: None,
        thinking_tokens: None,
        attachments: Vec::new(),
    }
}

fn config(base_url: String) -> OpenAiCompatConfig {
    OpenAiCompatConfig {
        name: "test",
        base_url,
        chat_path: "/v1/chat/completions".to_string(),
        auth: StaticBearer::new("sk-test"),
        extra_headers: vec![],
    }
}

#[tokio::test]
async fn records_then_replays_chat_without_network() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cassette_path = dir.path().join("openai-chat.json");
    let cassette_str = cassette_path.to_string_lossy().to_string();

    // --- Record phase: hit the mock server, capture to the cassette file. ---
    let server = MockServer::start().await;
    let base = server.uri();
    // An OpenAI-shape non-streaming response. A secret-looking token is embedded
    // in the assistant content so we can prove the recorded cassette is scrubbed
    // before it touches disk.
    let body = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "hello back key sk-live-abcdef0123456789abcdef0123456789"
            },
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 3, "completion_tokens": 2}
    });
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    // SAFETY: no other test in this binary reads ORIGIN_CASSETTE; the variable
    // is removed before the function returns so it cannot leak to siblings.
    std::env::set_var("ORIGIN_CASSETTE", format!("record:{cassette_str}"));
    let provider = OpenAiCompat::new(config(base.clone()));
    let resp = provider.chat(make_req()).await.expect("record chat");
    std::env::remove_var("ORIGIN_CASSETTE");

    assert_eq!(resp.assistant.blocks.len(), 1, "record path returns the response");
    assert!(cassette_path.exists(), "cassette file must be written on record");

    // The persisted body must be scrubbed: no live `sk-` token survives.
    let on_disk = std::fs::read_to_string(&cassette_path).expect("read cassette");
    assert!(
        !origin_cassette::contains_secret(&on_disk),
        "recorded body leaked a secret: {on_disk}"
    );
    assert!(on_disk.contains("***"), "expected redaction sentinel in cassette");

    // Drop the server so the socket is closed: a replay that touched the network
    // would now fail to connect. The cassette must serve the response from disk.
    drop(server);

    // --- Replay phase: server is gone; the recorded body is served from disk. ---
    // The cassette keys on (method, url), so we reuse the recorded base URL; the
    // replay path short-circuits before any socket is opened.
    std::env::set_var("ORIGIN_CASSETTE", format!("replay:{cassette_str}"));
    let provider = OpenAiCompat::new(config(base));
    let resp = provider.chat(make_req()).await.expect("replay chat");
    std::env::remove_var("ORIGIN_CASSETTE");

    assert_eq!(
        resp.assistant.blocks.len(),
        1,
        "replay returns the recorded block"
    );
    match &resp.assistant.blocks[0] {
        Block::Text { text, .. } => {
            // The scrubbed secret comes back as the redaction sentinel, never raw.
            assert!(
                text.starts_with("hello back"),
                "replay must reproduce the recorded assistant text (got: {text})"
            );
            assert!(
                !text.contains("sk-live"),
                "replayed text must not carry a live secret (got: {text})"
            );
        }
        other => panic!("expected text block, got {other:?}"),
    }
}
