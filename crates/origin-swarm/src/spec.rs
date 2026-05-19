//! Worker specifications, budgets, and small structured records (P9.6).
//!
//! These types are the input/output shapes the coordinator uses when launching
//! a worker and when a worker reports back. They are deliberately plain data —
//! no behavior, no lifetimes — so `CompletionReport` (in `report.rs`) can
//! bincode-encode them straight into the CAS without further marshalling.

use std::path::PathBuf;

use origin_plan::ActorId;
use serde::{Deserialize, Serialize};

/// Per-worker resource budget enforced by the coordinator.
///
/// In P9.6 the budget is recorded but only the **noop** worker is exercised
/// (which never spends any of it). P9.8's real worker loop is responsible for
/// checking the budget at each tool-call boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// Maximum wall-clock time for the entire worker, in milliseconds.
    pub max_wall_ms: u64,
    /// Maximum cumulative input tokens (provider-side).
    pub max_input_tokens: u64,
    /// Maximum cumulative output tokens (provider-side).
    pub max_output_tokens: u64,
    /// Maximum total tool invocations.
    pub max_tool_calls: u32,
}

impl Budget {
    /// Construct a budget. `#[must_use]` so callers don't drop the value.
    #[must_use]
    pub const fn new(
        max_wall_ms: u64,
        max_input_tokens: u64,
        max_output_tokens: u64,
        max_tool_calls: u32,
    ) -> Self {
        Self {
            max_wall_ms,
            max_input_tokens,
            max_output_tokens,
            max_tool_calls,
        }
    }
}

/// Input to `Coordinator::spawn` — describes a single worker to launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerSpec {
    /// Natural-language goal the worker should pursue.
    pub goal: String,
    /// Allow-list of tool names the worker may invoke.
    pub allowed_tools: Vec<String>,
    /// Resource ceiling.
    pub budget: Budget,
    /// Optional workspace root (`CoW` clone; P9.5). `None` in P9.6 tests.
    pub workspace: Option<PathBuf>,
    /// Parent actor id — used to thread Lamport ordering through worker ops.
    pub parent_actor: ActorId,
}

/// A follow-up task suggested by a worker on completion.
///
/// Carried inside [`crate::report::CompletionReport::follow_ups`]; the parent
/// coordinator can choose to dispatch each as a new `Task` (P9.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRef {
    /// Goal of the follow-up task.
    pub goal: String,
    /// Allow-listed tools for the follow-up worker.
    pub allowed_tools: Vec<String>,
}

/// Provider-side token / call accounting captured at worker exit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Cumulative provider input tokens.
    pub input_tokens: u64,
    /// Cumulative provider output tokens.
    pub output_tokens: u64,
    /// Number of tool calls actually made.
    pub tool_calls: u32,
}

/// Terminal status reported by a worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReportStatus {
    /// Worker reached its goal and is reporting success.
    Completed,
    /// Worker decided the goal could not be reached.
    GoalUnreachable,
    /// Worker hit a `Budget` cap.
    BudgetExhausted,
    /// Worker was killed externally (coordinator cancel / sandbox abort).
    Aborted,
}

/// Decision the worker logged with rationale, anchored to a Lamport timestamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// Lamport timestamp at which the decision was recorded.
    pub at_lamport: u64,
    /// Short description of the decision.
    pub decision: String,
    /// Reasoning behind the choice.
    pub rationale: String,
}
