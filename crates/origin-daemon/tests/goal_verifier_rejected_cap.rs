//! Bug #15 integration: after MAX_CONSECUTIVE_VERIFIER_REJECTIONS the
//! driver gives up and emits Cleared { VerifierRejected(..) }. Without
//! this cap a bogus main model that keeps claiming `met` against a
//! verifier that keeps rejecting would burn the user's full budget.

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::doc_markdown)]

use origin_goal::{ClearReasonWire, GoalState};

mod goal_helpers;
use goal_helpers::{run_driver_loop_with_state, ScriptedProvider};

#[tokio::test]
async fn three_consecutive_rejections_clear_with_verifier_rejected() {
    let provider = ScriptedProvider::new()
        // Iter 1: main claims met, verifier rejects.
        .with_main_reply(
            "done\n<goal-status state=\"met\"><reason>x</reason></goal-status>",
        )
        .with_verifier_reply("VERDICT: not_met — first rejection")
        // Iter 2: main claims met again, verifier rejects again.
        .with_main_reply(
            "really done\n<goal-status state=\"met\"><reason>y</reason></goal-status>",
        )
        .with_verifier_reply("VERDICT: not_met — second rejection")
        // Iter 3: main claims met yet again, verifier rejects, driver gives up.
        .with_main_reply(
            "no really\n<goal-status state=\"met\"><reason>z</reason></goal-status>",
        )
        .with_verifier_reply("VERDICT: not_met — third rejection");

    let state = GoalState::new("ship it".into(), Some(20), Some(1_000_000));
    let run = run_driver_loop_with_state(state, "go", &provider, 10).await;

    assert_eq!(run.iterations_run, 3);
    let (reason, iter, _tokens) = run.cleared().expect("driver must clear");
    match reason {
        ClearReasonWire::VerifierRejected { why } => {
            assert!(
                why.contains("third rejection"),
                "expected the final rejection reason to surface, got: {why:?}"
            );
        }
        other => panic!("expected VerifierRejected, got {other:?}"),
    }
    assert_eq!(iter, 3);
}
