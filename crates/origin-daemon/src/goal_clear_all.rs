// SPDX-License-Identifier: Apache-2.0
//! Helper for the `/clear`-clears-active-goal coupling (bug #10).
//!
//! `/clear` is a mechanical context reset ([`crate::protocol::ClientMessage::ClearAll`]),
//! not a skill activation. Its only stateful effect on the agent loop is that
//! it must terminate any active goal — otherwise the user told the daemon
//! "discard the in-session context" but `active_goal` still holds a
//! `GoalState` that the next `Prompt` would pick up.
//!
//! The wire enum already has [`origin_goal::ClearReasonWire::UserClearAll`]
//! for exactly this case — no other variant emits it. This module provides
//! the small, pure decision helper the daemon's `ClearAll` arm (via
//! `handle_clear_all`) calls to build the terminal `GoalCleared` event.

use crate::protocol::StreamEvent;
use origin_goal::{ClearReasonWire, GoalState};

/// Build the `/clear`-terminal [`StreamEvent::GoalCleared`] for the
/// current active-goal slot.
///
/// Returns `None` when no goal was active. The reason is always
/// [`ClearReasonWire::UserClearAll`] — the only event variant this helper
/// emits, distinguishing a `/clear`-induced terminal from a user
/// `/-goal`/Interrupt (`UserSlash`) or a driver cap-hit
/// (`MaxIter` / `BudgetExhausted`).
#[must_use]
pub fn clear_all_event_for(prior: Option<&GoalState>) -> Option<StreamEvent> {
    let g = prior?;
    Some(StreamEvent::GoalCleared {
        reason: ClearReasonWire::UserClearAll,
        iter: g.iter,
        tokens_spent: g.tokens_spent,
    })
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn no_active_goal_yields_no_event() {
        assert!(clear_all_event_for(None).is_none());
    }

    #[test]
    fn active_goal_yields_goal_cleared_user_clear_all() {
        let mut g = GoalState::new("ship it".into(), Some(5), Some(10_000));
        // Mutate counters so we can assert the snapshot reflects them.
        g.iter = 2;
        g.tokens_spent = 1_234;

        let ev = clear_all_event_for(Some(&g)).expect("must emit when goal active");
        match ev {
            StreamEvent::GoalCleared {
                reason,
                iter,
                tokens_spent,
            } => {
                assert!(
                    matches!(reason, ClearReasonWire::UserClearAll),
                    "must use UserClearAll reason, got {reason:?}"
                );
                assert_eq!(iter, 2, "iter must reflect the prior goal's counter");
                assert_eq!(
                    tokens_spent, 1_234,
                    "tokens_spent must reflect the prior goal's counter"
                );
            }
            other => panic!("expected GoalCleared, got {other:?}"),
        }
    }
}
