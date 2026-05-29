// SPDX-License-Identifier: Apache-2.0
//! Bug #3 integration: an empty/malformed Haiku reply must NOT silently
//! confirm the goal as Met. The driver should treat malformed output as
//! a retry (NotMet { unparseable }) instead of fail-open.
//!
//! Sequence: main model claims `met`, verifier returns garbage (no
//! VERDICT: line), driver synthesizes a retry prompt rather than emitting
//! GoalCleared { Met }.

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::needless_collect)]

use origin_daemon::protocol::StreamEvent;
use origin_goal::GoalState;

mod goal_helpers;
use goal_helpers::{run_driver_loop_with_state, ScriptedProvider};

#[tokio::test]
async fn malformed_verifier_reply_does_not_confirm_as_met() {
    let provider = ScriptedProvider::new()
        // Turn 1: main model claims met.
        .with_main_reply(
            "all done\n<goal-status state=\"met\"><reason>done</reason></goal-status>",
        )
        // Verifier returns garbage with no VERDICT: line. parse_verdict
        // returns Malformed; the driver MUST treat this as NotMet, not Met.
        .with_verifier_reply("I don't know.")
        // Turn 2: model addresses the gap and claims met legitimately.
        .with_main_reply(
            "really done now\n<goal-status state=\"met\"><reason>green</reason></goal-status>",
        )
        // Verifier confirms.
        .with_verifier_reply("VERDICT: met");

    let state = GoalState::new("get tests green".into(), None, None);
    let run = run_driver_loop_with_state(state, "go", &provider, 10).await;

    // The driver MUST iterate at least twice — first turn's malformed
    // verifier output must NOT clear the goal as Met.
    assert!(
        run.iterations_run >= 2,
        "expected at least 2 iterations after malformed verifier output; got {}",
        run.iterations_run
    );

    // The final event is GoalCleared but the path should have iterated
    // (not skipped straight to Met from malformed reply).
    let iteration_events: Vec<_> = run
        .events
        .iter()
        .filter(|e| matches!(e, StreamEvent::GoalIteration { .. }))
        .collect();
    assert!(
        !iteration_events.is_empty(),
        "malformed verifier output must trigger at least one GoalIteration event \
         (i.e. retry), not a direct GoalCleared. Events: {:?}",
        run.events
    );
}
