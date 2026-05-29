// SPDX-License-Identifier: Apache-2.0
//! Bug #8 integration: an activated goal must be checkpointed
//! immediately, not just after the first iteration completes. We exercise
//! the building blocks `handle_goal_activation` uses internally:
//!   1. parse args + build GoalState
//!   2. make_goal_checkpoint_token(session_id, 0, &Some(state))
//!   3. session_store.save_resume_token(&token)
//!   4. assert the on-disk token round-trips with the right condition.

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::doc_markdown)]

use origin_daemon::goal_checkpoint::make_goal_checkpoint_token;
use origin_daemon::session_store::SessionStore;
use origin_goal::{parse_goal_args, GoalState, GoalStatusWire};

#[test]
fn activation_checkpoint_persists_goal_immediately() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("sessions.db");

    // Step 1: parse + build, as the activation path does.
    let parsed = parse_goal_args("--max-iter=7 --budget=99k fix the leak")
        .expect("parse goal args");
    let state = GoalState::new(parsed.condition.clone(), parsed.max_iter, parsed.token_budget);

    // Step 2 + 3: checkpoint immediately. This is the call the bug fix
    // adds at the end of handle_goal_activation's success arm. Without
    // it, a crash between `/goal foo` and the first iteration would
    // lose the goal entirely.
    let session_id = "sess-activate-checkpoint";
    let token = make_goal_checkpoint_token(session_id, 0, &Some(state));
    {
        let store = SessionStore::open(&db_path).expect("open");
        store.save_resume_token(&token).expect("save");
    }

    // Step 4: re-open and verify the on-disk token carries the goal.
    let store2 = SessionStore::open(&db_path).expect("reopen");
    let loaded = store2
        .load_resume_token(session_id)
        .expect("load result")
        .expect("token must be present on disk after activation");

    let snap = loaded.goal.expect("goal snapshot must be present");
    assert_eq!(snap.condition, "fix the leak");
    assert_eq!(snap.max_iter, 7);
    assert_eq!(snap.token_budget, 99_000);
    assert_eq!(snap.iter, 0);
    assert_eq!(snap.tokens_spent, 0);
    // A freshly activated goal is Active, never Verifying or Cleared.
    assert_eq!(snap.status, GoalStatusWire::Active);
}
