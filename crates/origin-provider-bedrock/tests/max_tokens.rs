//! Verifies the bedrock provider asks for the same `max_tokens` ceiling as the
//! sibling `origin-provider-anthropic` crate. Bumped in commit 78916ea from
//! 4096 → `16_384`; bedrock serves the same Claude models, so it must match.

use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_bedrock::Bedrock;
use serde_json::json;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
#[allow(clippy::panic, clippy::unwrap_used)]
async fn bedrock_request_max_tokens_matches_anthropic_ceiling() {
    let server = MockServer::start().await;

    let response_body = json!({
        "content": [{"type": "text", "text": "ok"}],
        "usage": {"input_tokens": 1, "output_tokens": 1}
    });

    Mock::given(method("POST"))
        .and(path_regex(r"/model/.*/invoke"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
        .mount(&server)
        .await;

    let provider = Bedrock::new(
        server.uri(),
        "us-east-1",
        "anthropic.claude-3-haiku-20240307-v1:0",
        "AKIDEXAMPLE",
        "secretkey",
    );
    let req = ChatRequest {
        system: String::new(),
        messages: vec![Message::new(Role::User).with_block(Block::text("ping"))],
        model: "anthropic.claude-3-haiku-20240307-v1:0".into(),
        tools: vec![],
    };
    provider.chat(req).await.expect("bedrock chat should succeed");

    let received = server.received_requests().await.expect("wiremock recorded");
    assert_eq!(received.len(), 1, "expected exactly one outbound request");
    let body: serde_json::Value =
        serde_json::from_slice(&received[0].body).expect("request body should be JSON");
    let max_tokens = body
        .get("max_tokens")
        .and_then(serde_json::Value::as_u64)
        .expect("body must carry max_tokens");
    assert_eq!(
        max_tokens, 16_384,
        "bedrock max_tokens must match anthropic ceiling (was {max_tokens})",
    );
}
