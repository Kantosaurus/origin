//! Integration test T14: cumulative input+output tokens trigger the
//! `BudgetExhausted` cap.
//!
//! Setup: budget=120_000; each iteration's usage is (input=40_000,
//! output=10_000) = 50_000. After iter 3 cumulative is 150_000 > 120_000;
//! the next cap_check fires.

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::doc_markdown)]

use origin_goal::{ClearReasonWire, GoalState};

mod goal_helpers;
use goal_helpers::{run_driver_loop_with_state, ScriptedProvider};

#[tokio::test]
async fn cumulative_tokens_trigger_budget_cap() {
    let provider = ScriptedProvider::new()
        .with_main_reply_usage(
            "<goal-status state=\"in_progress\"><reason>x</reason></goal-status>",
            40_000,
            10_000,
        )
        .with_main_reply_usage(
            "<goal-status state=\"in_progress\"><reason>x</reason></goal-status>",
            40_000,
            10_000,
        )
        .with_main_reply_usage(
            "<goal-status state=\"in_progress\"><reason>x</reason></goal-status>",
            40_000,
            10_000,
        );

    let state = GoalState::new("stuff".into(), None, Some(120_000));
    let run = run_driver_loop_with_state(state, "begin", &provider, 50).await;

    // All three responses consumed before the cap fired.
    assert_eq!(run.iterations_run, 3);
    assert_eq!(provider.main_remaining(), 0);

    // Final event is BudgetExhausted, iter=3, tokens_spent>=120_000.
    let (reason, iter, tokens_spent) = run.cleared().expect("driver must clear");
    assert!(
        matches!(reason, ClearReasonWire::BudgetExhausted),
        "expected BudgetExhausted, got {reason:?}"
    );
    assert_eq!(iter, 3);
    assert!(
        tokens_spent >= 120_000,
        "tokens_spent must be at least the budget, got {tokens_spent}"
    );
    // Sanity: cumulative is exactly 3*(40k+10k) = 150_000 (no verifier
    // calls happened on in_progress).
    assert_eq!(tokens_spent, 150_000);
}
