use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_bedrock::Bedrock;
use serde_json::json;
use wiremock::matchers::{header_exists, method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
#[allow(clippy::panic)]
async fn signs_and_invokes_bedrock_model() {
    let server = MockServer::start().await;

    let response_body = json!({
        "content": [{"type": "text", "text": "hi"}],
        "usage": {"input_tokens": 3, "output_tokens": 2}
    });

    Mock::given(method("POST"))
        .and(path_regex(r"/model/.*/invoke"))
        .and(header_exists("authorization"))
        .and(header_exists("x-amz-date"))
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
    let resp = provider.chat(req).await.expect("bedrock chat should succeed");

    assert_eq!(resp.assistant.role, Role::Assistant);
    assert_eq!(resp.usage.input_tokens, 3);
    assert_eq!(resp.usage.output_tokens, 2);
    assert_eq!(resp.assistant.blocks.len(), 1);
    match &resp.assistant.blocks[0] {
        Block::Text { text, .. } => assert_eq!(text, "hi"),
        other => panic!("expected Text block, got {other:?}"),
    }
}
