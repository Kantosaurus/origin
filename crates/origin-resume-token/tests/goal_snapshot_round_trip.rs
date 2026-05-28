#![allow(clippy::unwrap_used)]

use origin_goal::{ClearReasonWire, GoalSnapshot, GoalStatusWire};
use origin_resume_token::ResumeToken;

#[test]
fn token_round_trips_with_active_goal() {
    let token = ResumeToken {
        session_id: "s1".into(),
        last_turn: 7,
        cas_handle_root: [0; 32],
        pending_tool_calls: vec![],
        plan_seq: 3,
        goal: Some(GoalSnapshot {
            condition: "do the thing".into(),
            iter: 4,
            max_iter: 20,
            tokens_spent: 12_345,
            token_budget: 200_000,
            started_at_unix: 1_716_000_000,
            status: GoalStatusWire::Active,
        }),
    };
    let bytes = serde_json::to_vec(&token).unwrap();
    let back: ResumeToken = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back.goal.as_ref().unwrap().condition, "do the thing");
    assert_eq!(back.goal.as_ref().unwrap().iter, 4);
    assert!(matches!(back.goal.unwrap().status, GoalStatusWire::Active));
}

#[test]
fn token_round_trips_without_goal_field_backward_compat() {
    // Old-format token bytes — no `goal` key.
    let raw = r#"{
        "session_id": "s1",
        "last_turn": 0,
        "cas_handle_root": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
        "pending_tool_calls": [],
        "plan_seq": 0
    }"#;
    let token: ResumeToken = serde_json::from_str(raw).unwrap();
    assert!(token.goal.is_none());
}

#[test]
fn token_round_trips_with_terminal_status() {
    let token = ResumeToken {
        session_id: "s1".into(),
        last_turn: 7,
        cas_handle_root: [0; 32],
        pending_tool_calls: vec![],
        plan_seq: 3,
        goal: Some(GoalSnapshot {
            condition: "x".into(),
            iter: 5,
            max_iter: 20,
            tokens_spent: 1_000,
            token_budget: 200_000,
            started_at_unix: 1_716_000_000,
            status: GoalStatusWire::Cleared { by: ClearReasonWire::MaxIter },
        }),
    };
    let bytes = serde_json::to_vec(&token).unwrap();
    let back: ResumeToken = serde_json::from_slice(&bytes).unwrap();
    assert!(matches!(
        back.goal.unwrap().status,
        GoalStatusWire::Cleared { by: ClearReasonWire::MaxIter }
    ));
}
