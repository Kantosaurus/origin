// SPDX-License-Identifier: Apache-2.0
//! B2 escape hatch: `ORIGIN_SYSTEM_CACHE=0` sends `system` as a plain string
//! with no marker (the pre-B2 wire shape) and gives all four `cache_control`
//! markers back to the messages — for Anthropic-compatible endpoints that
//! reject array-form or marked `system`.
//!
//! Its own test binary: it mutates a process-wide env var, and both states are
//! checked sequentially in one test so nothing else observes the toggle.

use origin_core::types::{Block, Message, Role};
use origin_planner::Plan;
use origin_provider::ChatRequest;
use origin_provider_anthropic::encode_request_with_plan_for_test;

fn request() -> (ChatRequest, Plan) {
    let plan = Plan::default();
    plan.set_dynamic_message_markers(vec![0, 1, 2, 3, 4, 5]);
    let req = ChatRequest {
        system: "you are origin".into(),
        messages: (0..6)
            .map(|i| Message {
                role: Role::User,
                blocks: vec![Block::text(format!("m{i}"))],
            })
            .collect(),
        model: "claude".into(),
        ..ChatRequest::default()
    };
    (req, plan)
}

fn markers(body: &serde_json::Value) -> usize {
    body.to_string().matches("\"cache_control\"").count()
}

#[test]
fn system_cache_env_switches_between_marked_array_and_plain_string() {
    let (req, plan) = request();

    // Default: marked block array, three message markers (four in total).
    std::env::remove_var("ORIGIN_SYSTEM_CACHE");
    let body = encode_request_with_plan_for_test(&req, &plan);
    assert!(
        body["system"].is_array(),
        "default system is a block array: {body}"
    );
    assert_eq!(markers(&body), 4);
    assert_eq!(markers(&body["messages"]), 3);

    // Opt-out: plain string, no system marker, four message markers.
    std::env::set_var("ORIGIN_SYSTEM_CACHE", "0");
    let body = encode_request_with_plan_for_test(&req, &plan);
    std::env::remove_var("ORIGIN_SYSTEM_CACHE");
    assert_eq!(body["system"], serde_json::json!("you are origin"));
    assert_eq!(markers(&body), 4);
    assert_eq!(markers(&body["messages"]), 4);
}
