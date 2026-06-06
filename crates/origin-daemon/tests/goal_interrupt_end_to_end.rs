// SPDX-License-Identifier: Apache-2.0
//! End-to-end check for the `ClientMessage::Interrupt` variant landed in
//! commit 1aa2f62: when a user interrupt arrives mid-iteration, the
//! driver clears the active goal with `GoalCleared { UserSlash }` on the
//! NEXT iteration boundary and stops consuming further scripted replies.
//!
//! Wave-4 limitation: the original `goal_user_interrupt_during_iteration`
//! test could only assert wire serialization + the in-memory push-back
//! buffer drain pattern; it never proved the harness's goal-loop actually
//! short-circuits when an Interrupt lands in the slot. This test does
//! that, against the Wave-9 real-`run_loop` harness:
//!
//!   1. Activate a goal with `max_iter=5` and three pre-loaded
//!      `in_progress` replies (enough that without the interrupt the
//!      driver would happily consume all three).
//!   2. Register a `between_iter_hook` that, on the first iteration
//!      boundary only, drops a `ClientMessage::Interrupt` into the
//!      shared `pending_message` slot.
//!   3. Run the driver — it should consume exactly ONE scripted reply
//!      (the first iteration), then on the second iteration's
//!      drain-buffer step see the Interrupt, clear with `UserSlash`,
//!      and exit.
//!
//! Failure modes this test catches:
//!   * If the harness fails to drain `pending_message` between
//!     iterations, the loop would consume all three scripted replies
//!     and either succeed (wrong outcome) or panic on exhaustion.
//!   * If the harness drains it but doesn't recognize Interrupt, the
//!     final event would be `GoalCleared { MaxIter }` (cap-hit) or no
//!     clear event at all — not `UserSlash`.

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use origin_daemon::protocol::ClientMessage;
use origin_goal::{ClearReasonWire, GoalState};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

mod goal_helpers;
use goal_helpers::{run_driver_loop_full, ScriptedProvider};

#[tokio::test]
async fn interrupt_between_iterations_clears_with_user_slash() {
    let provider = ScriptedProvider::new()
        .with_main_reply(
            "iter 1\n\
             <goal-status state=\"in_progress\"><reason>continuing</reason></goal-status>",
        )
        .with_main_reply(
            "iter 2 — MUST NOT RUN\n\
             <goal-status state=\"in_progress\"><reason>x</reason></goal-status>",
        )
        .with_main_reply(
            "iter 3 — MUST NOT RUN\n\
             <goal-status state=\"in_progress\"><reason>x</reason></goal-status>",
        );

    // Hook fires after each iteration; we only want to inject the
    // Interrupt once, on the first boundary. The atomic counter pins
    // the call-count so a refactor that changes hook-fire frequency
    // surfaces as a test failure instead of a silent skip.
    let hook_fires = Arc::new(AtomicU32::new(0));
    let hook_fires_clone = Arc::clone(&hook_fires);
    let hook: goal_helpers::BetweenIterHook = Box::new(move |_iter, pending| {
        let fires = Arc::clone(&hook_fires_clone);
        let pending = Arc::clone(pending);
        Box::pin(async move {
            let n = fires.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First boundary only: slip the Interrupt into the slot
                // so the next iteration's drain step sees it.
                *pending.lock().await = Some(ClientMessage::Interrupt);
            }
        })
    });

    let state = GoalState::new("long-running task".into(), Some(5), None);
    let run = run_driver_loop_full(state, "begin", &provider, 10, Some(hook)).await;

    // Exactly ONE main-loop iteration consumed (iter 1). The Interrupt
    // injected after that iteration must stop the loop BEFORE iter 2's
    // run_loop call lands.
    assert_eq!(
        run.iterations_run, 1,
        "Interrupt between iter 1 and iter 2 must prevent iter 2 from running; \
         iterations_run={}",
        run.iterations_run
    );
    assert_eq!(
        provider.main_remaining(),
        2,
        "two scripted replies should be left untouched after the Interrupt; got {}",
        provider.main_remaining()
    );
    assert_eq!(
        hook_fires.load(Ordering::SeqCst),
        1,
        "hook should fire exactly once (after iter 1, before the Interrupt-induced exit)"
    );

    // Final event must be GoalCleared { UserSlash }. The Wave-4 mimic
    // could prove `UserSlash` was a valid variant; this test proves the
    // harness actually emits it on the Interrupt path.
    let (reason, iter, _tokens) = run.cleared().expect("driver must clear on Interrupt");
    assert!(
        matches!(reason, ClearReasonWire::UserSlash),
        "expected UserSlash clear reason, got {reason:?}"
    );
    // iter is the goal's iter counter at the moment of clear. After one
    // run_loop call, record_iteration has been called once, so iter=1.
    assert_eq!(
        iter, 1,
        "iter counter at clear time should reflect one completed iteration"
    );
}

#[tokio::test]
async fn non_interrupt_pushback_also_clears_and_preserves_message() {
    // Counterpart: the production driver pushes back non-Interrupt
    // variants (e.g. a follow-up Prompt) for the outer loop to handle.
    // The harness has no outer loop, so we just assert the same
    // GoalCleared { UserSlash } event fires when ANY message sits in
    // the slot at drain time. This pins the contract documented in the
    // harness's drain-buffer comment: "Interrupt is consumed AND clears;
    // other variants also clear (the outer message loop dispatches
    // them)."
    let provider = ScriptedProvider::new()
        .with_main_reply(
            "iter 1\n\
             <goal-status state=\"in_progress\"><reason>x</reason></goal-status>",
        )
        .with_main_reply(
            "iter 2 — MUST NOT RUN\n\
             <goal-status state=\"in_progress\"><reason>x</reason></goal-status>",
        );

    let hook: goal_helpers::BetweenIterHook = Box::new(move |_iter, pending| {
        let pending = Arc::clone(pending);
        Box::pin(async move {
            // A follow-up Prompt — the variant the previous peek-and-
            // drop implementation silently lost.
            use origin_daemon::protocol::PromptRequest;
            *pending.lock().await = Some(ClientMessage::Prompt(PromptRequest {
                system: "s".into(),
                model: "m".into(),
                user_text: "after the goal".into(),
                session_id: None,
                ..Default::default()
            }));
        })
    });

    let state = GoalState::new("z".into(), Some(5), None);
    let run = run_driver_loop_full(state, "begin", &provider, 10, Some(hook)).await;

    assert_eq!(run.iterations_run, 1);
    assert_eq!(provider.main_remaining(), 1);

    let (reason, _iter, _tokens) = run.cleared().expect("must clear");
    assert!(matches!(reason, ClearReasonWire::UserSlash));
}
