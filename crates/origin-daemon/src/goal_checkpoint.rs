//! Goal-aware [`ResumeToken`] checkpoint writer.
//!
//! TODO(P14): `cas_handle_root`, `pending_tool_calls`, and `plan_seq` are
//! placeholders today — origin does not yet have a general per-turn session
//! checkpoint subsystem, so we cannot persist the full CAS root / in-flight
//! tool-call ids / plan sequence number per iteration. The goal-specific
//! snapshot below is correct on its own: a daemon-crash recovery path can
//! rehydrate the active `/goal` from `goal: Some(snapshot)` and pick up
//! where the iteration left off.
//!
//! When the session-checkpoint subsystem lands, fill in the three
//! placeholder fields and remove this notice. The goal field's shape will
//! not change.

use origin_resume_token::ResumeToken;

/// Build a goal-aware [`ResumeToken`] for a per-iteration checkpoint.
///
/// Fields:
/// * `session_id`     — the daemon's session id (one token file per session).
/// * `last_turn`      — the highest persisted turn index. Used by the resume
///                      handshake to cap the hydrated transcript.
/// * `active_goal`    — `&Option<GoalState>` snapshot at the call site.
///                      `None` produces a token with `goal: None`, valid for
///                      the broader resume contract but not goal-bearing.
///
/// The snapshot mirrors what `handle_resume_request` reads on the way back
/// in, so the round-trip is symmetric: write here, decode there, install in
/// the per-connection `active_goal` slot.
#[must_use]
pub fn make_goal_checkpoint_token(
    session_id: &str,
    last_turn: u32,
    active_goal: &Option<origin_goal::GoalState>,
) -> ResumeToken {
    ResumeToken {
        session_id: session_id.to_string(),
        last_turn,
        // Placeholders — see module-level TODO. These three fields belong to
        // a future general session-checkpoint subsystem that origin has not
        // built yet. A zero `cas_handle_root` is interpreted by the
        // supervisor as "no CAS hydration; replay from sqlite", which is
        // the same behaviour as a session with no resume token at all —
        // safe to ship as a goal-only checkpoint.
        cas_handle_root: [0u8; 32],
        pending_tool_calls: Vec::new(),
        plan_seq: 0,
        goal: active_goal.as_ref().map(|g| origin_goal::GoalSnapshot {
            condition: g.condition.clone(),
            iter: g.iter,
            max_iter: g.max_iter,
            tokens_spent: g.tokens_spent,
            token_budget: g.token_budget,
            started_at_unix: g
                .started_at
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            status: g.status.clone().into(),
        }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use origin_goal::{GoalState, GoalStatusWire};

    #[test]
    fn none_goal_produces_token_with_no_snapshot() {
        let token = make_goal_checkpoint_token("sess-1", 7, &None);
        assert_eq!(token.session_id, "sess-1");
        assert_eq!(token.last_turn, 7);
        assert!(token.goal.is_none());
        assert_eq!(token.cas_handle_root, [0u8; 32]);
        assert!(token.pending_tool_calls.is_empty());
        assert_eq!(token.plan_seq, 0);
    }

    #[test]
    fn active_goal_round_trips_into_snapshot() {
        let mut g = GoalState::new("fix tests".into(), Some(10), Some(50_000));
        g.iter = 3;
        g.tokens_spent = 1_234;
        let token = make_goal_checkpoint_token("sess-2", 4, &Some(g));
        let snap = token.goal.expect("goal snapshot present");
        assert_eq!(snap.condition, "fix tests");
        assert_eq!(snap.iter, 3);
        assert_eq!(snap.max_iter, 10);
        assert_eq!(snap.tokens_spent, 1_234);
        assert_eq!(snap.token_budget, 50_000);
        assert_eq!(snap.status, GoalStatusWire::Active);
    }
}
