// SPDX-License-Identifier: Apache-2.0
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, ToolSchema, Usage};
use origin_stream::{Ring, TokenKind};

struct FakeProv;

#[async_trait::async_trait]
impl Provider for FakeProv {
    fn name(&self) -> &'static str {
        "fake"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        Ok(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text("hi")),
            usage: Usage::default(),
        })
    }
}

#[tokio::test]
async fn fake_provider_round_trips() {
    let p = FakeProv;
    assert_eq!(p.name(), "fake");
    let req = ChatRequest {
        system: String::new(),
        messages: vec![Message::new(Role::User).with_block(Block::text("hello"))],
        model: "fake-1".to_string(),
        tools: Vec::<ToolSchema>::new(),
        effort: None,
        thinking_tokens: None,
        attachments: Vec::new(),
    };
    let resp = p.chat(req).await.expect("fake provider should not fail");
    assert_eq!(resp.assistant.role, Role::Assistant);
    assert_eq!(resp.assistant.blocks.len(), 1);
}

#[tokio::test]
async fn fake_provider_streams_one_token() {
    struct StreamProv;
    #[async_trait::async_trait]
    impl Provider for StreamProv {
        fn name(&self) -> &'static str {
            "stream"
        }
        async fn chat(&self, _: ChatRequest) -> Result<ChatResponse, ProviderError> {
            Err(ProviderError::Api(
                "non-streaming not supported in this test".into(),
            ))
        }
        async fn chat_stream(&self, _: ChatRequest, ring: &Ring) -> Result<(), ProviderError> {
            ring.publish(&origin_stream::TokenEvent::new(
                TokenKind::TextDelta,
                b"hi".to_vec(),
            ))
            .map_err(|e| ProviderError::Api(e.to_string()))?;
            ring.publish(&origin_stream::TokenEvent::new(TokenKind::TurnEnd, Vec::new()))
                .map_err(|e| ProviderError::Api(e.to_string()))?;
            ring.close();
            Ok(())
        }
    }

    let p = StreamProv;
    let ring = Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();
    p.chat_stream(
        ChatRequest {
            system: String::new(),
            messages: vec![],
            model: "stream-1".into(),
            tools: vec![],
            effort: None,
            thinking_tokens: None,
            attachments: Vec::new(),
        },
        &ring,
    )
    .await
    .expect("stream");

    let mut got = Vec::new();
    while let Some(ev) = sub.next().await.expect("recv") {
        got.push(ev);
    }
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].kind(), TokenKind::TextDelta);
    assert_eq!(got[1].kind(), TokenKind::TurnEnd);
}
