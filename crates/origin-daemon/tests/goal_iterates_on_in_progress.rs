//! Integration test T12: driver iterates on `in_progress`, calls verifier on
//! `met`, clears on verifier confirmation.
//!
//! Exercises the canonical happy path:
//! 1. Model emits 3 consecutive `in_progress` turns — driver synthesizes a
//!    `[goal-driver] Continue…` prompt for each.
//! 2. Model emits a `met` turn on the 4th — driver runs the verifier (one
//!    Haiku call) which confirms; driver clears with `Met { .. }`.
//!
//! Asserts 4 main-loop calls + 1 verifier call + final event is
//! `GoalCleared { Met }` with `iter == 4`.

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use origin_daemon::protocol::StreamEvent;
use origin_goal::{ClearReasonWire, GoalState};

mod goal_helpers;
use goal_helpers::{run_driver_loop_with_state, ScriptedProvider};

#[tokio::test]
async fn three_in_progress_then_met_with_verifier_confirm() {
    let provider = ScriptedProvider::new()
        .with_main_reply(
            "working on it\n\
             <goal-status state=\"in_progress\"><reason>step 1 of 3</reason></goal-status>",
        )
        .with_main_reply(
            "more progress\n\
             <goal-status state=\"in_progress\"><reason>step 2 of 3</reason></goal-status>",
        )
        .with_main_reply(
            "almost there\n\
             <goal-status state=\"in_progress\"><reason>step 3 of 3</reason></goal-status>",
        )
        .with_main_reply(
            "done!\n\
             <goal-status state=\"met\"><reason>tests green</reason></goal-status>",
        )
        .with_verifier_reply("VERDICT: met");

    let state = GoalState::new("get the tests green".into(), None, None);
    let run = run_driver_loop_with_state(state, "begin", &provider, 50).await;

    // 4 main-loop calls consumed.
    assert_eq!(run.iterations_run, 4);
    assert_eq!(provider.main_remaining(), 0);
    // Exactly one verifier call (only on the `met` claim).
    assert_eq!(provider.verifier_remaining(), 0);
    let verifier_calls = provider
        .captured()
        .into_iter()
        .filter(|r| r.is_verifier)
        .count();
    assert_eq!(verifier_calls, 1, "verifier should run exactly once on the met claim");

    // 3 in_progress turns produced 3 GoalIteration events; the 4th turn went
    // straight to verify → cleared.
    assert_eq!(run.iteration_count(), 3, "one GoalIteration per in_progress turn");
    let verifying_count = run
        .events
        .iter()
        .filter(|e| matches!(e, StreamEvent::GoalVerifying))
        .count();
    assert_eq!(verifying_count, 1);

    // Final event is GoalCleared { Met, iter: 4 }.
    let (reason, iter, _tokens) = run.cleared().expect("driver must clear");
    assert!(
        matches!(reason, ClearReasonWire::Met { .. }),
        "expected Met clear reason, got {reason:?}"
    );
    assert_eq!(iter, 4);
}
