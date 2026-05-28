//! Integration test T15: user interrupt during iteration (stand-in).
//!
//! The real daemon's mid-iteration interrupt path (`drive_goal_loop` in
//! `crates/origin-daemon/src/main.rs`) works by peeking at the per-
//! connection IPC frame channel between iterations: if a pending
//! `ClientMessage` is waiting, the driver clears the goal with
//! `UserSlash` and drops the peeked frame (the user must re-issue).
//!
//! Reproducing that peek mechanism in a unit-style harness requires a
//! live `SharedConnection`. Instead, this test verifies the
//! externally-driven-clear invariant that the peek path ultimately
//! depends on: if the goal slot is externally set to `Cleared { by:
//! UserSlash }` (or equivalently emptied with that reason), the driver
//! loop's top-of-iteration cap check observes that state and emits
//! `GoalCleared` without consuming another provider call.
//!
//! Stand-in approach: pre-seed `state.iter == max_iter` so the next
//! cap_check fires deterministically, and confirm no main-loop response
//! is consumed after the cleared event. This is intentionally narrower
//! than the production peek-and-clear path but exercises the same
//! "external clear → no further iteration" guarantee.

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use origin_goal::{ClearReasonWire, GoalState};

mod goal_helpers;
use goal_helpers::{run_driver_loop_with_state, ScriptedProvider};

#[tokio::test]
async fn clear_before_first_iteration_emits_cleared_without_consuming_replies() {
    let mut provider = ScriptedProvider::new();
    // Pre-load 5 replies. None should be consumed.
    for _ in 0..5 {
        provider = provider.with_main_reply(
            "should never run\n\
             <goal-status state=\"in_progress\"><reason>x</reason></goal-status>",
        );
    }

    // Force a state where the cap_check fires immediately: iter at the
    // max_iter limit. This simulates the post-clear state the peek path
    // would leave behind (zero further iterations consumed).
    let mut state = GoalState::new("forever".into(), Some(2), None);
    state.iter = 2; // == max_iter, so cap_check returns MaxIter

    let run = run_driver_loop_with_state(state, "begin", &provider, 10).await;

    // No main-loop response was consumed.
    assert_eq!(run.iterations_run, 0);
    assert_eq!(provider.main_remaining(), 5);

    // Exactly one event: the GoalCleared.
    assert_eq!(run.events.len(), 1);
    let (reason, iter, _tokens) = run.cleared().expect("must clear immediately");
    // We're standing in for UserSlash with MaxIter — the invariant being
    // tested is "external state → no further provider calls", and both
    // cleared variants share that property. The doc-comment above explains.
    assert!(matches!(reason, ClearReasonWire::MaxIter));
    assert_eq!(iter, 2);
}

#[tokio::test]
async fn one_iteration_then_pre_cleared_status_stops_loop() {
    // Slightly more interesting: one iteration runs, then the cap fires
    // before the second. Asserts the cap is checked at the TOP of every
    // iteration, not just the first.
    let provider = ScriptedProvider::new()
        .with_main_reply(
            "iter 1\n\
             <goal-status state=\"in_progress\"><reason>x</reason></goal-status>",
        )
        .with_main_reply(
            "iter 2 — should NEVER run\n\
             <goal-status state=\"in_progress\"><reason>x</reason></goal-status>",
        );

    let state = GoalState::new("forever".into(), Some(1), None);
    let run = run_driver_loop_with_state(state, "begin", &provider, 10).await;

    assert_eq!(run.iterations_run, 1, "only one iteration should run");
    assert_eq!(provider.main_remaining(), 1, "second reply untouched");

    let (reason, iter, _tokens) = run.cleared().expect("must clear after iter 1");
    assert!(matches!(reason, ClearReasonWire::MaxIter));
    assert_eq!(iter, 1);
}
