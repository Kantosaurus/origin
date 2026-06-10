// SPDX-License-Identifier: Apache-2.0
//! Verifies the Anthropic encoder emits the extended-thinking block only when a
//! budget is set, bumps `max_tokens` above the budget, and leaves the body
//! byte-identical (no `thinking` key, default `max_tokens`) when unset.
//!
//! Mirrors the existing `effort` provider test: `Some(n)` ⇒ the body has
//! `thinking.budget_tokens == n` and `max_tokens > n`; `None` ⇒ no `thinking`
//! key and the default `max_tokens` is preserved.

use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, ToolSchema};

/// The encoder's compiled-in default `max_tokens` (kept in sync with
/// `origin_provider_anthropic`'s `DEFAULT_MAX_TOKENS`). Used to assert the
/// unset path is byte-identical and that the set path reserves headroom on top
/// of the thinking budget.
const DEFAULT_MAX_TOKENS: u64 = 16_384;

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
        model: "claude-x".to_string(),
        tools: Vec::<ToolSchema>::new(),
        thinking_tokens,
        ..ChatRequest::default()
    }
}

#[test]
fn default_none_omits_thinking_and_keeps_default_max_tokens() {
    let body = origin_provider_anthropic::encode_request_for_test(&req_with(None));
    assert!(
        body.get("thinking").is_none(),
        "thinking key must be absent when thinking_tokens is None (byte-identical default)"
    );
    assert_eq!(
        body["max_tokens"].as_u64(),
        Some(DEFAULT_MAX_TOKENS),
        "max_tokens must stay at the compiled default when no thinking budget is set"
    );
}

#[test]
fn set_thinking_tokens_emits_budget_and_bumps_max_tokens() {
    let budget: u32 = 4_096;
    let body = origin_provider_anthropic::encode_request_for_test(&req_with(Some(budget)));

    assert_eq!(
        body["thinking"]["type"], "enabled",
        "thinking block must be enabled when a budget is set"
    );
    assert_eq!(
        body["thinking"]["budget_tokens"].as_u64(),
        Some(u64::from(budget)),
        "budget_tokens must echo the requested thinking budget"
    );

    let max_tokens = body["max_tokens"].as_u64().expect("max_tokens must be a number");
    assert!(
        max_tokens > u64::from(budget),
        "max_tokens ({max_tokens}) must exceed the thinking budget ({budget}) per Anthropic's rule"
    );
    // The encoder reserves a full default's worth of visible-output headroom.
    assert_eq!(max_tokens, u64::from(budget) + DEFAULT_MAX_TOKENS);
}

#[test]
fn large_budget_still_keeps_max_tokens_above_it() {
    // A budget at/above the default must still produce `max_tokens > budget`.
    let budget: u32 = 32_000;
    let body = origin_provider_anthropic::encode_request_for_test(&req_with(Some(budget)));
    let max_tokens = body["max_tokens"].as_u64().expect("number");
    assert!(
        max_tokens > u64::from(budget),
        "max_tokens must exceed a large budget too"
    );
}

#[test]
fn modern_model_uses_adaptive_thinking_without_budget_tokens() {
    // Claude 4.6/4.7/4.8 reject `{"type":"enabled","budget_tokens":n}` (a 400 on
    // 4.7+); the encoder must emit adaptive thinking instead. `claude-x` above
    // exercises the legacy enabled+budget branch for older models.
    let mut req = req_with(Some(8_192));
    req.model = "claude-opus-4-7".to_string();
    let body = origin_provider_anthropic::encode_request_for_test(&req);
    assert_eq!(
        body["thinking"]["type"], "adaptive",
        "claude-opus-4-7 must use adaptive thinking, not enabled+budget_tokens",
    );
    assert!(
        body["thinking"].get("budget_tokens").is_none(),
        "adaptive thinking must not carry budget_tokens",
    );
}
