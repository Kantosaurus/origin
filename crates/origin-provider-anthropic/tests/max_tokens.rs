//! Locks in the anthropic `max_tokens` ceiling at 16_384 (commit 78916ea).
//!
//! Pair with `origin-provider-bedrock/tests/max_tokens.rs` so a future bump
//! has to touch both crates in lockstep.

use origin_core::types::{Block, Message, Role};
use origin_provider::ChatRequest;
use origin_provider_anthropic::encode_request_for_test;

#[test]
fn anthropic_encoded_request_uses_16384_max_tokens() {
    let req = ChatRequest {
        system: String::new(),
        messages: vec![Message::new(Role::User).with_block(Block::text("hi"))],
        model: "claude-test".into(),
        tools: vec![],
    };
    let body = encode_request_for_test(&req);
    let max_tokens = body
        .get("max_tokens")
        .and_then(serde_json::Value::as_u64)
        .expect("encoded body must include max_tokens");
    assert_eq!(
        max_tokens, 16_384,
        "anthropic max_tokens must remain at 16_384 (was {max_tokens})",
    );
}
