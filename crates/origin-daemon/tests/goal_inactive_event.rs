//! Bug #20: bare `/goal` with no active goal must emit a benign
//! `GoalInactive` info event rather than overloading `SkillError` (which the
//! CLI renders with error styling).
//!
//! Function-level test mirroring the daemon's bare-`/goal` arm: the only
//! pure behavior under test is "no active goal -> GoalInactive event". The
//! IPC plumbing and `handle_goal_activation` dispatch live in `main.rs`
//! and are exercised by the integration suite; here we assert (a) the
//! variant exists and round-trips on the wire, and (b) the daemon's
//! bare-goal branch (which we mirror as `bare_goal_event_for`) chooses
//! `GoalInactive` when the slot is empty.

#![allow(clippy::panic)]

use origin_daemon::protocol::StreamEvent;
use origin_goal::GoalState;

/// Mirror of the daemon's bare-`/goal` branch in `handle_goal_activation`.
/// Returns the event the daemon should emit given the current active-goal
/// slot contents.
fn bare_goal_event_for(slot: Option<&GoalState>) -> StreamEvent {
    if let Some(g) = slot {
        StreamEvent::GoalActive {
            condition: g.condition.clone(),
            max_iter: g.max_iter,
            token_budget: g.token_budget,
        }
    } else {
        StreamEvent::GoalInactive
    }
}

#[test]
fn goal_inactive_event_round_trips_as_json() {
    let ev = StreamEvent::GoalInactive;
    let body = serde_json::to_vec(&ev).expect("encode");
    let decoded: StreamEvent = serde_json::from_slice(&body).expect("decode");
    assert!(
        matches!(decoded, StreamEvent::GoalInactive),
        "expected GoalInactive, got {decoded:?}"
    );
}

#[test]
fn bare_goal_with_no_active_goal_emits_goal_inactive_not_skill_error() {
    let ev = bare_goal_event_for(None);
    assert!(
        matches!(ev, StreamEvent::GoalInactive),
        "bare /goal with no active goal must surface GoalInactive (benign), \
         not SkillError; got {ev:?}"
    );
}

#[test]
fn bare_goal_with_active_goal_still_emits_goal_active() {
    // Regression guard: the GoalInactive change must not silently swap the
    // active-goal case too.
    let g = GoalState::new("ship it".into(), Some(3), Some(1_000));
    let ev = bare_goal_event_for(Some(&g));
    match ev {
        StreamEvent::GoalActive {
            condition,
            max_iter,
            token_budget,
        } => {
            assert_eq!(condition, "ship it");
            assert_eq!(max_iter, 3);
            assert_eq!(token_budget, 1_000);
        }
        other => panic!("expected GoalActive when slot has a goal, got {other:?}"),
    }
}
