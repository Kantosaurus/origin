use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_anthropic::Anthropic;
use origin_stream::{Ring, TokenKind};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn anthropic_streams_text_then_turn_end() {
    let server = MockServer::start().await;
    let sse_body = std::fs::read(format!(
        "{}/tests/fixtures/sse_hello.txt",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("fixture");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let prov = Anthropic::with_base_url("test-key", &server.uri());
    let ring = Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();

    let r = ring.clone();
    let prov_handle = tokio::spawn(async move {
        prov.chat_stream(
            ChatRequest {
                system: String::new(),
                messages: vec![Message::new(Role::User).with_block(Block::text("hi"))],
                model: "claude-test".into(),
                tools: vec![],
            },
            &r,
        )
        .await
        .expect("stream");
    });

    let mut text = String::new();
    let mut saw_turn_end = false;
    while let Some(ev) = sub.next().await.expect("recv") {
        match ev.kind() {
            TokenKind::TextDelta => {
                text.push_str(std::str::from_utf8(ev.payload()).expect("utf8"));
            }
            TokenKind::TurnEnd => saw_turn_end = true,
            _ => {}
        }
    }
    prov_handle.await.expect("prov task");
    assert_eq!(text, "Hello!");
    assert!(saw_turn_end);
}
