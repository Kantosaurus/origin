// SPDX-License-Identifier: Apache-2.0
//! Bug #7 contract test: when a goal is restored already at-or-past the
//! cap and the user sends a fresh Prompt, the daemon's `drive_goal_loop`
//! must (a) emit `GoalCleared { MaxIter }` AND (b) still let the user's
//! prompt go through to the model.
//!
//! This is a contract test at the type/shape level — the full
//! integration would require a live SharedConnection + SessionStore +
//! pre-loaded resume token, all of which the existing test harness
//! cannot easily produce. What we CAN test directly:
//!
//!   * GoalState::cap_check() returns `Some(MaxIter)` for the over-cap
//!     state we use in production.
//!   * After the cap fires and the slot is cleared (None), the goal-block
//!     render in `agent.rs:run_loop` will NOT emit a goal block (it
//!     filters by `Active|Verifying`).
//!
//! The behavioral assertion that the user's prompt is NOT silently
//! dropped is enforced by the fix in `main.rs::drive_goal_loop`: when
//! `cap_cleared_on_first_iter` is true, it explicitly calls run_loop
//! with `next_text` (the user's initial prompt). The test below pins
//! the cap_check contract that the production code relies on.

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::doc_markdown)]

use origin_goal::{ClearReason, GoalState};

#[test]
fn over_cap_state_returns_max_iter_from_cap_check() {
    let mut g = GoalState::new("x".into(), Some(3), None);
    g.iter = 5; // already past the cap
    assert!(matches!(g.cap_check(), Some(ClearReason::MaxIter)));
}

#[test]
fn over_budget_state_returns_budget_exhausted_from_cap_check() {
    let mut g = GoalState::new("x".into(), None, Some(100));
    g.tokens_spent = 500;
    assert!(matches!(g.cap_check(), Some(ClearReason::BudgetExhausted)));
}

#[test]
fn fresh_state_returns_no_cap_reason() {
    let g = GoalState::new("x".into(), None, None);
    assert!(g.cap_check().is_none());
}
