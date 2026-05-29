// SPDX-License-Identifier: Apache-2.0
//! Bug #10: `/clear` (the `clear` skill activation) must also clear an
//! active goal, emitting `GoalCleared { UserClearAll }`. Without the
//! coupling, a user who runs `/goal foo` and then `/clear` ends up with
//! the goal still active in the per-connection slot — the next `Prompt`
//! resumes the goal driver as if nothing had happened.
//!
//! The wire enum already had `ClearReasonWire::UserClearAll` reserved
//! for exactly this case; before bug #10's fix no variant emitted it.
//!
//! This test exercises the pure helper the daemon's
//! `ActivateSkill { name: "clear" }` arm now calls before falling through
//! to the normal skill catalog handler. The IPC dispatch arm itself lives
//! in `main.rs` (binary); integration coverage is provided by the
//! end-to-end suite. Here we pin the contract:
//!   * No active goal -> no event (clear is purely informational).
//!   * Active goal -> exactly one `GoalCleared { UserClearAll }` carrying
//!     the prior counters.

#![allow(clippy::panic)]

use origin_daemon::goal_clear_all::clear_all_event_for;
use origin_daemon::protocol::StreamEvent;
use origin_goal::{ClearReasonWire, GoalState};

#[test]
fn clear_with_no_active_goal_emits_no_event() {
    assert!(
        clear_all_event_for(None).is_none(),
        "clear with no active goal must be a no-op (no GoalCleared event)"
    );
}

#[test]
fn clear_with_active_goal_emits_user_clear_all() {
    let mut g = GoalState::new("write the readme".into(), Some(10), Some(50_000));
    g.iter = 4;
    g.tokens_spent = 12_345;

    let ev = clear_all_event_for(Some(&g)).expect("must emit when goal active");
    match ev {
        StreamEvent::GoalCleared {
            reason,
            iter,
            tokens_spent,
        } => {
            assert!(
                matches!(reason, ClearReasonWire::UserClearAll),
                "/clear must use UserClearAll (not UserSlash); got {reason:?}"
            );
            assert_eq!(iter, 4);
            assert_eq!(tokens_spent, 12_345);
        }
        other => panic!("expected GoalCleared, got {other:?}"),
    }
}

#[test]
fn user_clear_all_round_trips_on_wire() {
    // The reason variant has been on the wire since Wave 1; this is a
    // belt-and-braces check that the daemon's new emitter and the CLI's
    // new renderer agree on serialization.
    let ev = StreamEvent::GoalCleared {
        reason: ClearReasonWire::UserClearAll,
        iter: 1,
        tokens_spent: 99,
    };
    let body = serde_json::to_vec(&ev).expect("encode");
    let decoded: StreamEvent = serde_json::from_slice(&body).expect("decode");
    match decoded {
        StreamEvent::GoalCleared {
            reason,
            iter,
            tokens_spent,
        } => {
            assert!(matches!(reason, ClearReasonWire::UserClearAll));
            assert_eq!(iter, 1);
            assert_eq!(tokens_spent, 99);
        }
        other => panic!("decoded wrong variant: {other:?}"),
    }
}
