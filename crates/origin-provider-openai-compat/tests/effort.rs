// SPDX-License-Identifier: Apache-2.0
//! Verifies the `OpenAI`-compat encoder emits `reasoning_effort` only when set
//! AND the target model is a reasoning model (non-reasoning models like gpt-4o
//! 400 on the parameter), maps the internal level to a valid `OpenAI` value (no
//! `fast`/`max`, which `OpenAI` rejects), and is absent by default.

use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, ReasoningEffort, ToolSchema};
use origin_provider_openai_compat::wire::encode_request;

fn req_with_model(model: &str, effort: Option<ReasoningEffort>) -> ChatRequest {
    ChatRequest {
        system: String::new(),
        messages: vec![Message {
            role: Role::User,
            blocks: vec![Block::Text {
                text: "hi".to_string(),
                cache_marker: None,
            }],
        }],
        model: model.to_string(),
        tools: Vec::<ToolSchema>::new(),
        effort,
        thinking_tokens: None,
        attachments: Vec::new(),
    }
}

fn body_of(model: &str, effort: Option<ReasoningEffort>, stream: bool) -> serde_json::Value {
    serde_json::to_value(encode_request(&req_with_model(model, effort), stream)).expect("serializes")
}

#[test]
fn default_none_omits_reasoning_effort_key() {
    let body = body_of("o3-mini", None, false);
    assert!(
        body.get("reasoning_effort").is_none(),
        "reasoning_effort must be absent when effort is None (byte-identical default)"
    );
}

#[test]
fn non_reasoning_model_never_sends_reasoning_effort() {
    // gpt-4o rejects reasoning_effort with a 400; it must be omitted even when an
    // effort level is set.
    let body = body_of("gpt-4o", Some(ReasoningEffort::High), false);
    assert!(
        body.get("reasoning_effort").is_none(),
        "reasoning_effort must be omitted for non-reasoning models like gpt-4o"
    );
}

#[test]
fn reasoning_model_emits_mapped_valid_value() {
    // Reasoning models (o-series, gpt-5, *-thinking, *-reasoner) get the hint,
    // mapped to a value OpenAI accepts. `fast`/`max` are never sent verbatim.
    assert_eq!(body_of("o3-mini", Some(ReasoningEffort::Medium), false)["reasoning_effort"], "medium");
    assert_eq!(body_of("gpt-5", Some(ReasoningEffort::Low), true)["reasoning_effort"], "low");
    assert_eq!(
        body_of("o4-mini", Some(ReasoningEffort::Fast), false)["reasoning_effort"], "low",
        "Fast must map to `low` (OpenAI has no `fast`)",
    );
    assert_eq!(
        body_of("deepseek-reasoner", Some(ReasoningEffort::Max), false)["reasoning_effort"], "high",
        "Max must map to `high` (OpenAI has no `max`)",
    );
}
