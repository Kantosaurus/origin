//! Wire-shape types shared between the protocol (origin-daemon) and the
//! resume token (origin-resume-token). Kept in this crate so both consumers
//! depend on `origin-goal` rather than each other.

use crate::state::{ClearReason, GoalStatus, TagOutcome};
use serde::{Deserialize, Serialize};

#[allow(clippy::module_name_repetitions)] // `TagOutcomeWire` mirrors `TagOutcome` for protocol/resume use
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TagOutcomeWire {
    Met,
    InProgress { what_remains: String },
    Blocked { why: String },
    Missing,
}

impl From<TagOutcome> for TagOutcomeWire {
    fn from(t: TagOutcome) -> Self {
        match t {
            TagOutcome::Met => Self::Met,
            TagOutcome::InProgress { what_remains } => Self::InProgress { what_remains },
            TagOutcome::Blocked { why } => Self::Blocked { why },
            TagOutcome::Missing => Self::Missing,
        }
    }
}

#[allow(clippy::module_name_repetitions)] // `ClearReasonWire` mirrors `ClearReason` for protocol/resume use
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClearReasonWire {
    UserSlash,
    UserClearAll,
    MaxIter,
    BudgetExhausted,
    VerifierRejected { why: String },
    Met { reason: String },
    VerifierUnavailable,
    Blocked { why: String },
}

impl From<ClearReason> for ClearReasonWire {
    fn from(r: ClearReason) -> Self {
        match r {
            ClearReason::UserSlash => Self::UserSlash,
            ClearReason::UserClearAll => Self::UserClearAll,
            ClearReason::MaxIter => Self::MaxIter,
            ClearReason::BudgetExhausted => Self::BudgetExhausted,
            ClearReason::VerifierRejected(why) => Self::VerifierRejected { why },
            ClearReason::Met { reason } => Self::Met { reason },
            ClearReason::VerifierUnavailable => Self::VerifierUnavailable,
            ClearReason::Blocked { why } => Self::Blocked { why },
        }
    }
}

#[allow(clippy::module_name_repetitions)] // `GoalStatusWire` mirrors `GoalStatus` for protocol/resume use
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GoalStatusWire {
    Active,
    Verifying,
    Met { reason: String },
    Cleared { by: ClearReasonWire },
}

impl From<GoalStatus> for GoalStatusWire {
    fn from(s: GoalStatus) -> Self {
        match s {
            GoalStatus::Active => Self::Active,
            GoalStatus::Verifying => Self::Verifying,
            GoalStatus::Met { reason } => Self::Met { reason },
            GoalStatus::Cleared { by } => Self::Cleared { by: by.into() },
        }
    }
}

/// Snapshot persisted in the resume token. The `started_at_unix` field
/// avoids round-tripping `SystemTime` (whose serde shape is host-dependent).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoalSnapshot {
    pub condition: String,
    pub iter: u32,
    pub max_iter: u32,
    pub tokens_spent: u64,
    pub token_budget: u64,
    pub started_at_unix: u64,
    pub status: GoalStatusWire,
    /// Preserved across resume so the driver's first post-resume tick
    /// knows whether the model last claimed `Met` (triggers a fresh
    /// verifier call) vs. ordinary in-progress. `serde(default)` so
    /// pre-Phase-9-fix tokens without this field still deserialize. Bug #9.
    #[serde(default)]
    pub last_status_tag: Option<TagOutcomeWire>,
}
