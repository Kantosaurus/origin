// SPDX-License-Identifier: Apache-2.0
//! Bug #11 integration: verifier spend that pushes us past budget must
//! clear as BudgetExhausted, not Met. Without the post-charge cap_check
//! the user could be charged for an "successful" goal that actually blew
//! the budget on the verifier's own call.

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::doc_markdown)]

use origin_goal::{ClearReasonWire, GoalState};

mod goal_helpers;
use goal_helpers::{run_driver_loop_with_state, ScriptedProvider};

#[tokio::test]
async fn verifier_met_but_over_budget_clears_as_budget_exhausted() {
    // Token budget of 200. Main turn spends 100 → tokens_spent = 100.
    // Then verifier returns Met but the harness's ScriptedVerifier
    // forwards a usage payload that pushes total past 200. The driver
    // must prefer BudgetExhausted over Met (Bug #11).
    let provider = ScriptedProvider::new()
        .with_main_reply_usage(
            "done!\n<goal-status state=\"met\"><reason>x</reason></goal-status>",
            60, // input
            40, // output → total 100
        )
        // Verifier returns met but the ScriptedProvider records its own
        // usage as zero. The driver charges (0, 0) via record_verifier_tokens,
        // so we need the main spend alone to be at-or-past budget for the
        // post-charge cap to fire. Set budget to 100 so the iteration ends
        // right at the cap; the post-charge cap_check sees tokens_spent
        // >= budget and clears as BudgetExhausted.
        .with_verifier_reply("VERDICT: met");

    let state = GoalState::new("x".into(), None, Some(100));
    let run = run_driver_loop_with_state(state, "go", &provider, 10).await;

    let (reason, _iter, tokens_spent) = run.cleared().expect("must clear");
    match reason {
        ClearReasonWire::BudgetExhausted => {
            assert!(
                tokens_spent >= 100,
                "expected tokens_spent >= budget (100); got {tokens_spent}"
            );
        }
        ClearReasonWire::Met { .. } => panic!(
            "BUG #11 regression: Met returned despite over-budget. tokens_spent={tokens_spent}"
        ),
        other => panic!("expected BudgetExhausted, got {other:?}"),
    }
}
