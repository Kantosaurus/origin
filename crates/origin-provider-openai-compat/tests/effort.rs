// SPDX-License-Identifier: Apache-2.0
//! Verifies the `OpenAI`-compat encoder emits `reasoning_effort` only when set,
//! and that the default (`None`) leaves the body free of that key.

use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, ReasoningEffort, ToolSchema};
use origin_provider_openai_compat::wire::encode_request;

fn req_with(effort: Option<ReasoningEffort>) -> ChatRequest {
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
        effort,
        thinking_tokens: None,
        attachments: Vec::new(),
    }
}

#[test]
fn default_none_omits_reasoning_effort_key() {
    let req = req_with(None);
    let wire = encode_request(&req, false);
    let body = serde_json::to_value(&wire).expect("wire serializes");
    assert!(
        body.get("reasoning_effort").is_none(),
        "reasoning_effort must be absent when effort is None (byte-identical default)"
    );
}

#[test]
fn set_effort_emits_reasoning_effort() {
    let req = req_with(Some(ReasoningEffort::Medium));
    let wire = encode_request(&req, false);
    let body = serde_json::to_value(&wire).expect("wire serializes");
    assert_eq!(body["reasoning_effort"], "medium");

    let req = req_with(Some(ReasoningEffort::Low));
    let wire = encode_request(&req, true);
    let body = serde_json::to_value(&wire).expect("wire serializes");
    assert_eq!(body["reasoning_effort"], "low");
}
