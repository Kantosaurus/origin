//! Integration test T13: when the model never reports `met`, the driver
//! must stop at exactly `max_iter` provider calls and emit `GoalCleared {
//! MaxIter }`.

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use origin_goal::{ClearReasonWire, GoalState};

mod goal_helpers;
use goal_helpers::{run_driver_loop_with_state, ScriptedProvider};

#[tokio::test]
async fn always_in_progress_caps_at_max_iter() {
    let mut provider = ScriptedProvider::new();
    // Pre-load way more replies than the cap so a buggy driver would
    // happily consume them instead of stopping.
    for _ in 0..50 {
        provider = provider.with_main_reply(
            "still working\n\
             <goal-status state=\"in_progress\"><reason>more</reason></goal-status>",
        );
    }

    let state = GoalState::new("keep at it".into(), Some(3), None);
    let run = run_driver_loop_with_state(state, "begin", &provider, 100).await;

    // Driver must have stopped at exactly 3 main-loop calls.
    assert_eq!(run.iterations_run, 3);
    assert_eq!(provider.main_remaining(), 47, "remaining replies untouched");

    // Final event is GoalCleared { MaxIter, iter: 3 }.
    let (reason, iter, _tokens) = run.cleared().expect("driver must clear");
    assert!(
        matches!(reason, ClearReasonWire::MaxIter),
        "expected MaxIter reason, got {reason:?}"
    );
    assert_eq!(iter, 3);
}
