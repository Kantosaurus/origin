#![allow(clippy::panic)]

use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider, ProviderError};
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

#[tokio::test]
async fn anthropic_stream_429_returns_rate_limit_error() {
    // Before this fix, every non-success HTTP status from the streaming path
    // flattened into `ProviderError::Api("status 429 …")`, so any upstream
    // retry-on-RateLimit logic was silently bypassed.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "7")
                .set_body_string(
                    r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#,
                ),
        )
        .mount(&server)
        .await;

    let prov = Anthropic::with_base_url("test-key", &server.uri());
    let ring = Ring::with_capacity(64 * 1024);
    let err = prov
        .chat_stream(
            ChatRequest {
                system: String::new(),
                messages: vec![Message::new(Role::User).with_block(Block::text("hi"))],
                model: "claude-test".into(),
                tools: vec![],
            },
            &ring,
        )
        .await
        .expect_err("must surface 429 as ProviderError");

    match err {
        ProviderError::RateLimit {
            retry_after_secs,
            message,
        } => {
            assert_eq!(retry_after_secs, 7);
            assert_eq!(message, "slow down");
        }
        other => panic!("expected RateLimit, got {other:?}"),
    }
}

#[tokio::test]
async fn anthropic_stream_401_returns_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_string("nope"))
        .mount(&server)
        .await;

    let prov = Anthropic::with_base_url("test-key", &server.uri());
    let ring = Ring::with_capacity(64 * 1024);
    let err = prov
        .chat_stream(
            ChatRequest {
                system: String::new(),
                messages: vec![Message::new(Role::User).with_block(Block::text("hi"))],
                model: "claude-test".into(),
                tools: vec![],
            },
            &ring,
        )
        .await
        .expect_err("must surface 401 as ProviderError::Auth");

    assert!(matches!(err, ProviderError::Auth), "expected Auth, got {err:?}");
}
