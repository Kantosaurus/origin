//! After Phase 11 handle-substitution, cache markers may appear on any
//! message, not just `msg_idx == 0`. Confirm `encode_request_for_test`
//! emits `cache_control` on the marker block regardless of message position.

use origin_core::types::{Block, CacheBoundary, Message, Role};
use origin_provider::ChatRequest;
use origin_provider_anthropic::encode_request_for_test;

#[test]
fn cache_marker_on_non_first_message_is_emitted() {
    let m0 = Message {
        role: Role::User,
        blocks: vec![Block::Text {
            text: "a".into(),
            cache_marker: None,
        }],
    };
    let m1 = Message {
        role: Role::User,
        blocks: vec![Block::Text {
            text: "b".into(),
            cache_marker: Some(CacheBoundary::Sticky),
        }],
    };
    let req = ChatRequest {
        system: String::new(),
        messages: vec![m0, m1],
        model: "claude".into(),
        tools: vec![],
    };
    let body = encode_request_for_test(&req);
    let s = serde_json::to_string(&body).expect("serialize");
    assert!(
        s.contains(r#""cache_control":{"type":"ephemeral"}"#),
        "cache_control missing on msg 1: {s}"
    );

    // Specifically: the marker must land on message 1's first block (not msg 0).
    let messages = body["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 2, "expected two messages");
    assert!(
        messages[0]["content"][0].get("cache_control").is_none(),
        "msg 0 must not have cache_control (no marker on its block)"
    );
    assert!(
        messages[1]["content"][0].get("cache_control").is_some(),
        "msg 1 must have cache_control (marker on its block)"
    );
}
