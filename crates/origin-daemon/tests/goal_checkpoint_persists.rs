//! Integration test: goal-aware `ResumeToken` checkpoint round-trip.
//!
//! `drive_goal_loop` calls `make_goal_checkpoint_token` after every
//! iteration and on terminal-clear, then writes the resulting token via
//! `SessionStore::save_resume_token`. This test exercises the same
//! pipeline end-to-end against a tempdir-backed `SessionStore`:
//!
//!   1. Build a `GoalState` with non-default counters (so the snapshot
//!      values are distinguishable from defaults).
//!   2. Construct the token via the production helper.
//!   3. Persist it.
//!   4. Load it back from a freshly-opened `SessionStore` (a different
//!      handle so we exercise the on-disk format end-to-end, not just
//!      an in-memory round-trip).
//!   5. Assert every field of the loaded `GoalSnapshot` matches.
//!
//! Crash-recovery contract: when the daemon restarts, the supervisor
//! reads any `<state_dir>/resume/*.json` tokens and replays each one as
//! a `ClientMessage::ResumeRequest`. The daemon then re-installs the
//! goal via `handle_resume_request`. The round-trip below covers the
//! write half of that contract; the read half is exercised by the
//! resume integration tests under `tests/resume_session.rs`.

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use origin_daemon::goal_checkpoint::make_goal_checkpoint_token;
use origin_daemon::session_store::SessionStore;
use origin_goal::{GoalState, GoalStatusWire};

#[test]
fn token_with_active_goal_round_trips_through_session_store() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("sessions.db");

    // Build a non-trivial GoalState. The driver's call site passes the
    // current snapshot at the moment of checkpoint, so we set
    // tokens_spent and iter to non-default values to make the round-trip
    // observable.
    let mut g = GoalState::new("close all open PRs".into(), Some(15), Some(75_000));
    g.iter = 4;
    g.tokens_spent = 12_345;
    let goal_opt = Some(g);

    let token = make_goal_checkpoint_token("sess-checkpoint-A", 9, &goal_opt);

    // Open the store, save the token, drop the store handle. Re-open
    // from the same path to prove the round-trip goes through disk.
    {
        let store = SessionStore::open(&db_path).expect("open store");
        store.save_resume_token(&token).expect("save token");
    }
    let store2 = SessionStore::open(&db_path).expect("reopen store");
    let loaded = store2
        .load_resume_token("sess-checkpoint-A")
        .expect("load result")
        .expect("token present");

    assert_eq!(loaded.session_id, "sess-checkpoint-A");
    assert_eq!(loaded.last_turn, 9);
    assert_eq!(loaded.cas_handle_root, [0u8; 32]); // placeholder until session-checkpoint subsystem lands
    assert!(loaded.pending_tool_calls.is_empty());
    assert_eq!(loaded.plan_seq, 0);

    let snap = loaded.goal.expect("goal snapshot present");
    assert_eq!(snap.condition, "close all open PRs");
    assert_eq!(snap.iter, 4);
    assert_eq!(snap.max_iter, 15);
    assert_eq!(snap.tokens_spent, 12_345);
    assert_eq!(snap.token_budget, 75_000);
    // Fresh `GoalState::new(..)` starts in Active.
    assert_eq!(snap.status, GoalStatusWire::Active);
}

#[test]
fn token_with_no_active_goal_persists_with_none_snapshot() {
    // After a terminal clear `drive_goal_loop` may write a token with
    // `goal: None` to surface "no active goal" to the next resume
    // handshake. Verify that shape round-trips too.
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("sessions.db");

    let token = make_goal_checkpoint_token("sess-checkpoint-B", 3, &None);
    {
        let store = SessionStore::open(&db_path).expect("open store");
        store.save_resume_token(&token).expect("save token");
    }
    let store2 = SessionStore::open(&db_path).expect("reopen store");
    let loaded = store2
        .load_resume_token("sess-checkpoint-B")
        .expect("load result")
        .expect("token present");

    assert_eq!(loaded.session_id, "sess-checkpoint-B");
    assert_eq!(loaded.last_turn, 3);
    assert!(loaded.goal.is_none());
}
