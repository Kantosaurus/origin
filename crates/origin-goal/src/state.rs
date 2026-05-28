//! Goal state machine.
//!
//! The driver lives in `origin-daemon`; this crate only carries the types
//! and the pure-function transitions so they can be unit-tested without
//! tokio or providers.

use serde::{Deserialize, Serialize};
use std::time::SystemTime;

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
    pub last_status_tag: Option<TagOutcome>,
}

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
            last_status_tag: None,
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
