// SPDX-License-Identifier: Apache-2.0
//! Anthropic's Messages API rejects any request carrying more than 4 blocks
//! with `cache_control` (`invalid_request_error: "A maximum of 4 blocks with
//! cache_control may be provided. Found 5."`). The wire encoder is the last
//! place that controls what is sent, so it MUST never emit more than 4 markers
//! no matter how the upstream marker-selection paths overlap.
//!
//! These tests reproduce the production 400 two ways:
//!   1. The index-based `dynamic_message_markers` path selecting >4 messages.
//!   2. The exact production drift: block-carried markers on the latest turn
//!      boundaries PLUS a `dynamic_message_markers` list computed in the
//!      pre-strip index space, where `strip_orphan_tool_results` drops a turn
//!      and shifts the path-3 indices off their intended blocks.

use origin_core::types::{Block, CacheBoundary, Message, Role};
use origin_planner::Plan;
use origin_provider::ChatRequest;
use origin_provider_anthropic::encode_request_with_plan_for_test;

/// Count every `cache_control` object across the serialized request body — this
/// is exactly what Anthropic counts when it enforces the 4-marker ceiling.
fn count_cache_control(body: &serde_json::Value) -> usize {
    body["messages"].as_array().map_or(0, |msgs| {
        msgs.iter()
            .filter_map(|m| m["content"].as_array())
            .flatten()
            .filter(|block| block.get("cache_control").is_some())
            .count()
    })
}

/// Which message indices ended up carrying a `cache_control` marker.
fn marked_message_indices(body: &serde_json::Value) -> Vec<usize> {
    body["messages"].as_array().map_or_else(Vec::new, |msgs| {
        msgs.iter()
            .enumerate()
            .filter_map(|(i, m)| {
                let has = m["content"]
                    .as_array()
                    .is_some_and(|cs| cs.iter().any(|c| c.get("cache_control").is_some()));
                has.then_some(i)
            })
            .collect()
    })
}

fn user_text(s: &str) -> Message {
    Message {
        role: Role::User,
        blocks: vec![Block::text(s)],
    }
}

#[test]
fn request_never_emits_more_than_four_cache_control_markers() {
    // Six messages, each flagged for a path-3 (`dynamic_message_markers`)
    // marker. Naively that is six `cache_control` blocks — one past the ceiling
    // by two. The encoder must clamp to 4.
    let messages: Vec<Message> = (0..6).map(|i| user_text(&format!("m{i}"))).collect();
    let plan = Plan::default();
    plan.set_dynamic_message_markers(vec![0, 1, 2, 3, 4, 5]);

    let req = ChatRequest {
        system: String::new(),
        messages,
        model: "claude".into(),
        tools: vec![],
        effort: None,
        thinking_tokens: None,
        attachments: Vec::new(),
    };
    let body = encode_request_with_plan_for_test(&req, &plan);

    let n = count_cache_control(&body);
    assert!(
        n <= 4,
        "wire request must carry at most 4 cache_control markers (Anthropic \
         rejects more); got {n} in {body}"
    );

    // Policy: when markers overflow, keep the LATEST ones — newer prefixes
    // amortize the cache across the most subsequent turns. So messages 2..=5
    // survive and 0,1 are dropped.
    assert_eq!(
        marked_message_indices(&body),
        vec![2, 3, 4, 5],
        "the four surviving markers must be the latest four messages"
    );
}

#[test]
fn strip_dropping_a_turn_does_not_overflow_the_ceiling() {
    // Faithful reproduction of the production 400. The daemon marked the latest
    // four turn boundaries at the BLOCK level (path 2) and mirrored those same
    // message indices into `dynamic_message_markers` (path 3), both computed
    // against `session.messages`. But message index 1 here is an orphan tool
    // turn (its tool_result has no matching tool_use), which
    // `strip_orphan_tool_results` removes — shifting every later message down by
    // one. Path 2's markers ride along with their blocks (still correct), while
    // path 3's now-stale indices land on different blocks, so the union exceeds
    // four. The encoder must still cap the wire at 4.
    let mut messages: Vec<Message> = Vec::new();
    // 0: a well-formed opening user turn.
    messages.push(user_text("start"));
    // 1: an ORPHAN tool result — strip drops this whole message.
    messages.push(Message {
        role: Role::Tool,
        blocks: vec![Block::ToolResult {
            tool_use_id: "missing".into(),
            handle: None,
            inline: Some(b"orphan".to_vec()),
            cache_marker: None,
        }],
    });
    // 2..=9: four well-formed assistant(tool_use)/tool(tool_result) turns. The
    // latest four tool_result blocks carry block-level markers (path 2).
    for t in 0..4 {
        let id = format!("call{t}");
        messages.push(Message {
            role: Role::Assistant,
            blocks: vec![Block::ToolUse {
                id: id.clone(),
                name: "Read".into(),
                input_json: b"{}".to_vec(),
                cache_marker: None,
            }],
        });
        messages.push(Message {
            role: Role::Tool,
            blocks: vec![Block::ToolResult {
                tool_use_id: id,
                handle: None,
                inline: Some(b"ok".to_vec()),
                cache_marker: Some(CacheBoundary::Sticky),
            }],
        });
    }

    // Path 3: indices into the PRE-strip list pointing at the four marked
    // tool_result messages (indices 3, 5, 7, 9). After strip removes index 1,
    // these indices are off by one and drift onto the wrong blocks.
    let plan = Plan::default();
    plan.set_dynamic_message_markers(vec![3, 5, 7, 9]);

    let req = ChatRequest {
        system: String::new(),
        messages,
        model: "claude".into(),
        tools: vec![],
        effort: None,
        thinking_tokens: None,
        attachments: Vec::new(),
    };
    let body = encode_request_with_plan_for_test(&req, &plan);

    let n = count_cache_control(&body);
    assert!(
        n <= 4,
        "index drift from a stripped orphan turn must not push cache_control \
         past Anthropic's 4-marker ceiling; got {n} in {body}"
    );
}
