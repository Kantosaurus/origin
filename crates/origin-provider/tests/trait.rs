use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, ToolSchema, Usage};

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
    };
    let resp = p.chat(req).await.expect("fake provider should not fail");
    assert_eq!(resp.assistant.role, Role::Assistant);
    assert_eq!(resp.assistant.blocks.len(), 1);
}
