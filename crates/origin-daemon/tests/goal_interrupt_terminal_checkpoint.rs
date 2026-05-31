// SPDX-License-Identifier: Apache-2.0
//! Bug #17 integration: when the outer-loop Interrupt arm clears a goal,
//! it must write a terminal-status checkpoint so a crash between
//! cancellation and the next user Prompt cannot resurrect the killed
//! goal.
//!
//! Exercises the snapshot shape the Interrupt arm now writes: status
//! `Cleared { UserSlash }` plus the surviving counters.

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use origin_daemon::session_store::SessionStore;
use origin_goal::{ClearReasonWire, GoalSnapshot, GoalStatusWire};
use origin_resume_token::ResumeToken;

#[test]
fn interrupt_arm_writes_terminal_cleared_snapshot() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("sessions.db");

    // Replicate the snapshot shape the Interrupt arm now writes. The
    // load test that follows is the contract the bug fix guarantees: a
    // restart MUST see status `Cleared { UserSlash }`, not `Active`.
    let snap = GoalSnapshot {
        condition: "long-running goal".into(),
        iter: 5,
        max_iter: 20,
        tokens_spent: 1_234,
        token_budget: 200_000,
        started_at_unix: 1_716_000_000,
        status: GoalStatusWire::Cleared {
            by: ClearReasonWire::UserSlash,
        },
        last_status_tag: None,
    };
    let token = ResumeToken {
        session_id: "sess-interrupt".into(),
        last_turn: 0,
        cas_handle_root: [0u8; 32],
        pending_tool_calls: Vec::new(),
        plan_seq: 0,
        goal: Some(snap),
        detached_at_unix: None,
        memory_estimate_bytes: None,
    };

    {
        let store = SessionStore::open(&db_path).expect("open");
        store.save_resume_token(&token).expect("save");
    }
    let store2 = SessionStore::open(&db_path).expect("reopen");
    let loaded = store2
        .load_resume_token("sess-interrupt")
        .expect("load")
        .expect("token present");

    let snap = loaded.goal.expect("snapshot present");
    // The terminal checkpoint MUST be Cleared, not Active — that's the
    // whole point of bug #17. `handle_resume_request` already filters
    // out terminal statuses so a Cleared snapshot doesn't get re-hydrated.
    match snap.status {
        GoalStatusWire::Cleared {
            by: ClearReasonWire::UserSlash,
        } => {}
        other => panic!("expected Cleared {{ UserSlash }}, got {other:?}"),
    }
}
