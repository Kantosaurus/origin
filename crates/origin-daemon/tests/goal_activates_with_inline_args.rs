//! Integration test T11: `/goal <args>` parser + initial `GoalState`
//! together produce the right activation payload.
//!
//! Wave-3/Wave-4 status note: there is no `TestDaemon` harness yet — daemon
//! integration tests use scripted providers + direct `run_loop` calls (see
//! `loop_e2e.rs`). This test therefore exercises the activation pipeline at
//! the function level: parse-args → build `GoalState` → assert the values the
//! daemon would put into a `StreamEvent::GoalActive`. That is the actual
//! piece of logic the daemon's `ActivateSkill { name: "goal", args }` arm
//! performs (see `crates/origin-daemon/src/main.rs`).

#![allow(clippy::unwrap_used)]

use origin_daemon::protocol::StreamEvent;
use origin_goal::{parse_goal_args, GoalState};

/// Helper: replicate the daemon's `ActivateSkill -> GoalActive` translation.
fn activate(args: &str) -> StreamEvent {
    let parsed = parse_goal_args(args).expect("parse_goal_args");
    let state = GoalState::new(parsed.condition.clone(), parsed.max_iter, parsed.token_budget);
    StreamEvent::GoalActive {
        condition: state.condition,
        max_iter: state.max_iter,
        token_budget: state.token_budget,
    }
}

#[test]
fn goal_activate_emits_goal_active_with_defaults() {
    let ev = activate("fix the failing tests");
    match ev {
        StreamEvent::GoalActive { condition, max_iter, token_budget } => {
            assert_eq!(condition, "fix the failing tests");
            assert_eq!(max_iter, 20);
            assert_eq!(token_budget, 200_000);
        }
        other => panic!("expected GoalActive, got {other:?}"),
    }
}

#[test]
fn goal_activate_with_flags_uses_overrides() {
    let ev = activate("--max-iter=5 --budget=50k fix tests");
    match ev {
        StreamEvent::GoalActive { condition, max_iter, token_budget } => {
            assert_eq!(condition, "fix tests");
            assert_eq!(max_iter, 5);
            assert_eq!(token_budget, 50_000);
        }
        other => panic!("expected GoalActive, got {other:?}"),
    }
}

#[test]
fn goal_state_defaults_match_event_defaults() {
    // Crosscheck: the constants the daemon would advertise must match what
    // GoalState actually enforces during a run.
    let g = GoalState::new("anything".into(), None, None);
    assert_eq!(g.max_iter, 20);
    assert_eq!(g.token_budget, 200_000);
    assert_eq!(g.iter, 0);
    assert_eq!(g.tokens_spent, 0);
    assert!(g.last_status_tag.is_none());
}
