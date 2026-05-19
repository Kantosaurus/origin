//! Live smoke test against Anthropic. Skipped unless ANTHROPIC_API_KEY is set.

#[tokio::test(flavor = "current_thread")]
async fn live_smoke() {
    let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") else {
        eprintln!("ANTHROPIC_API_KEY not set; skipping live_smoke");
        return;
    };
    use origin_core::types::{Block, Message, Role};
    use origin_provider::{ChatRequest, Provider};
    use origin_provider_anthropic::Anthropic;

    let provider = Anthropic::new(api_key);
    let req = ChatRequest {
        system: "Reply with the single word OK and nothing else.".into(),
        messages: vec![Message::new(Role::User).with_block(Block::text("ping"))],
        model: "claude-opus-4-7".into(),
        tools: vec![],
    };
    let resp = provider.chat(req).await.expect("anthropic should answer");
    let txt = match &resp.assistant.blocks[0] {
        Block::Text { text, .. } => text.clone(),
        other => panic!("expected text, got {other:?}"),
    };
    assert!(!txt.is_empty(), "anthropic should return non-empty text");
    println!("live_smoke reply: {txt}");
}
