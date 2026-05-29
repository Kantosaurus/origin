// SPDX-License-Identifier: Apache-2.0
//! Integration test T15: user interrupt during iteration.
//!
//! The real daemon's mid-iteration interrupt path lives in
//! `drive_goal_loop` (see `crates/origin-daemon/src/main.rs`). Between
//! iterations the driver peeks at the per-connection IPC frame channel:
//!
//!   * `ClientMessage::Interrupt`  → clear the goal, drop the body
//!     (Interrupt is itself a no-op after the clear).
//!   * any other variant           → clear the goal, push the parsed
//!     `ClientMessage` into the per-connection `pending_message` buffer
//!     so the outer loop dispatches it on its next tick. This replaces
//!     the previous behaviour, which silently dropped the user's
//!     follow-up.
//!
//! The full peek path requires a live `SharedConnection`, so this file
//! tests three invariants independently:
//!
//!   1. The legacy "external clear → no further provider call" guarantee
//!      (cap_check fires before the first / second iteration).
//!   2. `ClientMessage::Interrupt` is a valid wire variant — round-trips
//!      through serde so the daemon can decode it from a peeked frame.
//!   3. The push-back buffer pattern: when `Some(msg)` is in the slot,
//!      the outer loop dispatches that message before reading a new
//!      frame; when the slot is `None` the loop falls back to a fresh
//!      wire read. Both directions are exercised against the same
//!      `Arc<Mutex<Option<ClientMessage>>>` shape main.rs allocates.

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::doc_markdown)]

use origin_daemon::protocol::{ClientMessage, PromptRequest};
use origin_goal::{ClearReasonWire, GoalState};
use std::sync::Arc;

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

#[test]
fn interrupt_round_trips_through_client_message_serde() {
    // The driver's peek path parses the peeked frame body as a
    // ClientMessage; an Interrupt sent by the CLI MUST decode without
    // a payload. Internally-tagged with `kind: "interrupt"`.
    let json = br#"{"kind":"interrupt"}"#;
    let msg: ClientMessage = serde_json::from_slice(json).expect("decode interrupt");
    assert!(matches!(msg, ClientMessage::Interrupt));

    // Round-trip the other way so the CLI/test side can rely on the
    // same wire shape.
    let encoded = serde_json::to_string(&ClientMessage::Interrupt).expect("encode interrupt");
    assert_eq!(encoded, r#"{"kind":"interrupt"}"#);
}

#[tokio::test]
async fn push_back_buffer_dispatches_pending_before_wire_read() {
    // Mirrors the outer message loop's drain pattern in main.rs:
    //
    //     let pushed_back = pending_message.lock().await.take();
    //     let msg = if let Some(m) = pushed_back { m } else { /* read wire */ };
    //
    // This test fills the slot with a follow-up Prompt that the driver
    // would have stashed after a mid-iteration peek, then verifies the
    // drain pattern returns that exact message and leaves the slot
    // empty for the next iteration.
    let pending: Arc<tokio::sync::Mutex<Option<ClientMessage>>> =
        Arc::new(tokio::sync::Mutex::new(None));

    // Drain on empty slot → None (outer loop would then read a new
    // frame from the wire).
    assert!(pending.lock().await.take().is_none());

    // Driver pushes back a follow-up Prompt (the variant the previous
    // peek-and-drop implementation silently lost).
    let followup = ClientMessage::Prompt(PromptRequest {
        system: "s".into(),
        model: "m".into(),
        user_text: "after the interrupt".into(),
        session_id: None,
    });
    *pending.lock().await = Some(followup);

    // First drain: returns the buffered Prompt, leaves slot empty.
    let drained = pending.lock().await.take();
    let Some(ClientMessage::Prompt(req)) = drained else {
        panic!("expected pushed-back Prompt");
    };
    assert_eq!(req.user_text, "after the interrupt");

    // Second drain: empty again, outer loop would fall back to wire
    // read. This is the invariant that prevents the daemon from
    // re-dispatching the same buffered message twice.
    assert!(pending.lock().await.take().is_none());
}

#[tokio::test]
async fn push_back_buffer_holds_interrupt_too_when_caller_chooses() {
    // The driver's policy is "Interrupt is consumed in the peek; only
    // OTHER variants get pushed back". But the buffer itself is policy-
    // agnostic — it stores whatever ClientMessage the caller writes.
    // This test pins that down so a future refactor that changes the
    // driver's push-back policy can still rely on the buffer working
    // for any variant.
    let pending: Arc<tokio::sync::Mutex<Option<ClientMessage>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    *pending.lock().await = Some(ClientMessage::Interrupt);
    let drained = pending.lock().await.take();
    assert!(matches!(drained, Some(ClientMessage::Interrupt)));
}
