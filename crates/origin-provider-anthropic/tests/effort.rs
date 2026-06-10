// SPDX-License-Identifier: Apache-2.0
//! Verifies the Anthropic encoder emits effort as `output_config.effort` (NOT a
//! top-level `effort` field, which the Messages API rejects with 400) only when
//! set, maps the internal level to a valid API value (`fast` -> `low`), and
//! leaves the body free of both keys when effort is `None`.

use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, ReasoningEffort, ToolSchema};

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
        model: "claude-x".to_string(),
        tools: Vec::<ToolSchema>::new(),
        effort,
        thinking_tokens: None,
        attachments: Vec::new(),
    }
}

#[test]
fn default_none_omits_effort_keys() {
    let body = origin_provider_anthropic::encode_request_for_test(&req_with(None));
    assert!(
        body.get("output_config").is_none() && body.get("effort").is_none(),
        "no effort/output_config key when effort is None (byte-identical default)"
    );
}

#[test]
fn set_effort_emits_output_config_effort_with_valid_value() {
    // Must be nested under output_config, never top-level (top-level 400s).
    let body = origin_provider_anthropic::encode_request_for_test(&req_with(Some(ReasoningEffort::High)));
    assert!(
        body.get("effort").is_none(),
        "must NOT send a top-level effort field"
    );
    assert_eq!(body["output_config"]["effort"], "high");

    // `fast` is not a valid Anthropic effort value — it maps to `low`.
    let body = origin_provider_anthropic::encode_request_for_test(&req_with(Some(ReasoningEffort::Fast)));
    assert_eq!(
        body["output_config"]["effort"], "low",
        "Fast must map to the valid `low` level (Anthropic has no `fast`)",
    );

    // `max` is valid for Anthropic (Opus tier).
    let body = origin_provider_anthropic::encode_request_for_test(&req_with(Some(ReasoningEffort::Max)));
    assert_eq!(body["output_config"]["effort"], "max");
}
