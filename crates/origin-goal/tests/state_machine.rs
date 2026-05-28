#![allow(clippy::unwrap_used)]

use origin_goal::{ClearReason, GoalState, TagOutcome};

#[test]
fn defaults_when_args_omitted() {
    let g = GoalState::new("fix tests".into(), None, None);
    assert_eq!(g.condition, "fix tests");
    assert_eq!(g.iter, 0);
    assert_eq!(g.max_iter, 20);
    assert_eq!(g.token_budget, 200_000);
    assert_eq!(g.tokens_spent, 0);
    assert!(g.last_status_tag.is_none());
}

#[test]
fn cap_check_clean_on_fresh_state() {
    let g = GoalState::new("x".into(), None, None);
    assert_eq!(g.cap_check(), None);
}

#[test]
fn cap_check_fires_on_max_iter_equality() {
    let mut g = GoalState::new("x".into(), Some(3), None);
    g.iter = 3;
    assert_eq!(g.cap_check(), Some(ClearReason::MaxIter));
}

#[test]
fn cap_check_fires_on_budget_equality() {
    let mut g = GoalState::new("x".into(), None, Some(100));
    g.tokens_spent = 100;
    assert_eq!(g.cap_check(), Some(ClearReason::BudgetExhausted));
}

#[test]
fn record_iteration_accumulates_tokens_and_increments_iter() {
    let mut g = GoalState::new("x".into(), None, None);
    g.record_iteration(50, 25, TagOutcome::InProgress { what_remains: "a".into() });
    assert_eq!(g.tokens_spent, 75);
    assert_eq!(g.iter, 1);
    assert_eq!(
        g.last_status_tag,
        Some(TagOutcome::InProgress { what_remains: "a".into() })
    );
}

#[test]
fn record_verifier_tokens_charges_to_same_budget() {
    let mut g = GoalState::new("x".into(), None, Some(1_000));
    g.record_verifier_tokens(400, 100);
    assert_eq!(g.tokens_spent, 500);
    assert_eq!(g.iter, 0); // verifier doesn't count as an iteration
}

#[test]
fn budget_overshoot_one_iteration_then_caps() {
    let mut g = GoalState::new("x".into(), None, Some(100));
    assert_eq!(g.cap_check(), None);              // can run once
    g.record_iteration(80, 60, TagOutcome::InProgress { what_remains: String::new() });
    // tokens_spent = 140, over budget; next cap check fires
    assert_eq!(g.cap_check(), Some(ClearReason::BudgetExhausted));
}

#[test]
fn saturating_arithmetic_does_not_panic() {
    let mut g = GoalState::new("x".into(), Some(u32::MAX), Some(u64::MAX));
    g.record_iteration(u64::MAX, u64::MAX, TagOutcome::Met);
    assert_eq!(g.tokens_spent, u64::MAX);
}
