// SPDX-License-Identifier: Apache-2.0
//! Verifies that `ChatRequest.thinking_tokens` is a no-op for the
//! `OpenAI`-compatible encoder: extended-thinking `budget_tokens` is an
//! Anthropic-only concept with no Chat Completions equivalent, so the encoded
//! body must be byte-identical whether or not a thinking budget was requested,
//! and must never carry a `thinking` (or `budget_tokens`) key.

use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, ToolSchema};
use origin_provider_openai_compat::wire::encode_request;

fn req_with(thinking_tokens: Option<u32>) -> ChatRequest {
    ChatRequest {
        system: String::new(),
        messages: vec![Message {
            role: Role::User,
            blocks: vec![Block::Text {
                text: "hi".to_string(),
                cache_marker: None,
            }],
        }],
        model: "gpt-x".to_string(),
        tools: Vec::<ToolSchema>::new(),
        thinking_tokens,
        ..ChatRequest::default()
    }
}

#[test]
fn thinking_tokens_is_a_noop_on_the_wire() {
    let none_body =
        serde_json::to_value(encode_request(&req_with(None), false)).expect("serialize");
    let some_body =
        serde_json::to_value(encode_request(&req_with(Some(8_192)), false)).expect("serialize");

    // The two bodies must be identical: thinking-tokens does not perturb the
    // OpenAI-compat wire at all.
    assert_eq!(
        none_body, some_body,
        "thinking_tokens must not change the OpenAI-compat request body"
    );
    // And neither carries an Anthropic-style thinking control.
    assert!(some_body.get("thinking").is_none());
    assert!(some_body.get("budget_tokens").is_none());
}
