//! Goal state machine.
//!
//! The driver lives in `origin-daemon`; this crate only carries the types
//! and the pure-function transitions so they can be unit-tested without
//! tokio or providers.

use serde::{Deserialize, Serialize};
use std::time::{Instant, SystemTime};

pub const DEFAULT_MAX_ITER: u32 = 20;
pub const DEFAULT_TOKEN_BUDGET: u64 = 200_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TagOutcome {
    Met,
    InProgress { what_remains: String },
    Blocked { why: String },
    Missing,
}

#[allow(clippy::module_name_repetitions)] // `GoalState` is the documented public type
#[derive(Debug, Clone)]
pub struct GoalState {
    pub condition: String,
    pub status: GoalStatus,
    pub iter: u32,
    pub max_iter: u32,
    pub tokens_spent: u64,
    pub token_budget: u64,
    pub started_at: SystemTime,
    /// Monotonic counterpart to `started_at` for elapsed-time math. Not
    /// serialized — when a `GoalState` is rebuilt from a snapshot on resume
    /// the wall-clock origin is unknown, so we default this to
    /// `Instant::now()` (i.e. "started now"). Bug #25.
    pub started_at_instant: Instant,
    pub last_status_tag: Option<TagOutcome>,
    /// Number of CONSECUTIVE verifier rejections so far. Reset to 0 on any
    /// non-`Met` tag or a successful verification; incremented when the
    /// verifier returns `Verdict::NotMet`. When it reaches
    /// [`MAX_CONSECUTIVE_VERIFIER_REJECTIONS`] the driver gives up and
    /// emits `Cleared { VerifierRejected(..) }`. Bug #15.
    pub consecutive_rejections: u32,
}

/// Cap on consecutive verifier rejections before the driver gives up.
pub const MAX_CONSECUTIVE_VERIFIER_REJECTIONS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalStatus {
    Active,
    Verifying,
    Met { reason: String },
    Cleared { by: ClearReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClearReason {
    UserSlash,
    UserClearAll,
    MaxIter,
    BudgetExhausted,
    VerifierRejected(String),
    Met { reason: String },
    VerifierUnavailable,
    /// The main model reported the goal blocked on something that requires the
    /// human. The driver clears the goal so the loop stops and the user can
    /// respond, instead of spinning on a block it cannot resolve autonomously.
    Blocked { why: String },
}

impl GoalState {
    #[must_use]
    pub fn new(condition: String, max_iter: Option<u32>, token_budget: Option<u64>) -> Self {
        Self {
            condition,
            status: GoalStatus::Active,
            iter: 0,
            max_iter: max_iter.unwrap_or(DEFAULT_MAX_ITER),
            tokens_spent: 0,
            token_budget: token_budget.unwrap_or(DEFAULT_TOKEN_BUDGET),
            started_at: SystemTime::now(),
            started_at_instant: Instant::now(),
            last_status_tag: None,
            consecutive_rejections: 0,
        }
    }

    /// Top-of-iteration cap check. Returns `Some(reason)` if the iteration
    /// should not run; the caller emits `GoalCleared { reason }` and stops.
    #[must_use]
    pub const fn cap_check(&self) -> Option<ClearReason> {
        if self.iter >= self.max_iter {
            Some(ClearReason::MaxIter)
        } else if self.tokens_spent >= self.token_budget {
            Some(ClearReason::BudgetExhausted)
        } else {
            None
        }
    }

    /// Called after `run_loop` returns. Adds tokens, increments iter, and
    /// records the parsed tag.
    pub fn record_iteration(&mut self, input_tokens: u64, output_tokens: u64, tag: TagOutcome) {
        self.tokens_spent = self
            .tokens_spent
            .saturating_add(input_tokens.saturating_add(output_tokens));
        self.iter = self.iter.saturating_add(1);
        self.last_status_tag = Some(tag);
    }

    /// Bookkeeping for the verifier's own token spend.
    pub fn record_verifier_tokens(&mut self, input_tokens: u64, output_tokens: u64) {
        self.tokens_spent = self
            .tokens_spent
            .saturating_add(input_tokens.saturating_add(output_tokens));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // Bug #25: `started_at_instant` is monotonic — elapsed-time math
    // works even if the wall clock jumps backwards. Test that
    // elapsed() returns a sensible positive duration.
    #[test]
    fn started_at_instant_is_monotonic_and_measures_elapsed() {
        let g = GoalState::new("test".into(), None, None);
        let captured = g.started_at_instant;
        std::thread::sleep(std::time::Duration::from_millis(10));
        let elapsed = captured.elapsed();
        assert!(
            elapsed >= std::time::Duration::from_millis(10),
            "started_at_instant.elapsed() must measure >= 10ms after sleep; got {elapsed:?}"
        );
    }

    #[test]
    fn fresh_goal_starts_with_zero_consecutive_rejections() {
        let g = GoalState::new("test".into(), None, None);
        assert_eq!(g.consecutive_rejections, 0);
    }
}
