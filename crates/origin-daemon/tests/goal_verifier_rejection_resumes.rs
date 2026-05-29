// SPDX-License-Identifier: Apache-2.0
//! Integration test T16: verifier rejection resumes iteration with the
//! verifier's reason injected into the next user prompt.
//!
//! Sequence:
//! 1. Main model emits `met` (prematurely).
//! 2. Verifier returns `VERDICT: not_met — tests still failing`.
//! 3. Driver synthesizes a `[goal-driver] You claimed the goal was met,
//!    but the verifier disagreed: tests still failing. ...` prompt.
//! 4. Main model emits `met` again on the next turn (now legitimately).
//! 5. Verifier returns `VERDICT: met` → driver clears with `Met`.
//!
//! Asserts: 2 main-loop calls + 2 verifier calls; the second main-loop
//! call's user message contains the verifier's rejection reason; final
//! event is `GoalCleared { Met }` with `iter == 2`.

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use origin_goal::{ClearReasonWire, GoalState};

mod goal_helpers;
use goal_helpers::{run_driver_loop_with_state, ScriptedProvider};

#[tokio::test]
async fn verifier_rejection_injects_reason_and_resumes() {
    let provider = ScriptedProvider::new()
        // Turn 1: model claims met prematurely.
        .with_main_reply(
            "I think we're done\n\
             <goal-status state=\"met\"><reason>looks done to me</reason></goal-status>",
        )
        // Verifier rejects with a specific reason.
        .with_verifier_reply("VERDICT: not_met — tests still failing")
        // Turn 2: model addresses the gap, claims met again.
        .with_main_reply(
            "fixed it\n\
             <goal-status state=\"met\"><reason>green now</reason></goal-status>",
        )
        // Verifier confirms.
        .with_verifier_reply("VERDICT: met");

    let state = GoalState::new("get tests green".into(), None, None);
    let run = run_driver_loop_with_state(state, "go", &provider, 50).await;

    assert_eq!(run.iterations_run, 2);
    assert_eq!(provider.main_remaining(), 0);
    assert_eq!(provider.verifier_remaining(), 0);

    // Two GoalVerifying events (one per `met` claim).
    let verifying_count = run
        .events
        .iter()
        .filter(|e| matches!(e, origin_daemon::protocol::StreamEvent::GoalVerifying))
        .count();
    assert_eq!(verifying_count, 2);

    // The captured request log must show the second main-loop call's user
    // message contains the verifier's rejection reason. Filter to non-
    // verifier requests; the second one is the resumed turn.
    let main_calls: Vec<_> = provider
        .captured()
        .into_iter()
        .filter(|r| !r.is_verifier)
        .collect();
    assert_eq!(main_calls.len(), 2);
    let resumed_prompt = &main_calls[1].last_user_text;
    assert!(
        resumed_prompt.contains("tests still failing"),
        "resumed prompt must inject the verifier's reason; got: {resumed_prompt:?}"
    );
    assert!(
        resumed_prompt.starts_with("[goal-driver]"),
        "resumed prompt must be a driver-synthesized continuation; got: {resumed_prompt:?}"
    );

    // Final event: GoalCleared { Met, iter: 2 }.
    let (reason, iter, _tokens) = run.cleared().expect("driver must clear on second met");
    assert!(
        matches!(reason, ClearReasonWire::Met { .. }),
        "expected Met clear reason, got {reason:?}"
    );
    assert_eq!(iter, 2);
}
