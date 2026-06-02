// SPDX-License-Identifier: Apache-2.0
//! `Task` builtin — dispatch a sub-agent worker via `origin-swarm`,
//! await its [`CompletionReport`](origin_swarm::CompletionReport), and inline
//! the actionable view (P9.8, N7.5).
//!
//! The full report stays in CAS via its `transcript_handle`; this tool only
//! returns the structured fields a parent agent typically needs to keep
//! reasoning: `status`, `summary`, `files_touched` (hex-encoded CAS handles),
//! and `follow_ups` (the goal strings of any suggested follow-up tasks).

// `Task{Input,Output,Budget,Error}` repeat the module name on purpose: the
// public surface is documented vocab the agent loop and JSON schemas refer to
// (see plan §P9.8). Renaming to `Input` / `Output` etc. would clash with the
// sibling builtin modules' types when re-exported.
#![allow(clippy::module_name_repetitions)]

use origin_swarm::{Budget, Coordinator, ReportStatus, SwarmError, WorkerHandle, WorkerSpec};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const fn default_wall_ms() -> u64 {
    60_000
}
const fn default_input_tokens() -> u64 {
    32_000
}
const fn default_output_tokens() -> u64 {
    8_000
}
const fn default_tool_calls() -> u32 {
    32
}

/// Per-task resource budget. Mirrors [`origin_swarm::Budget`] but with serde
/// defaults so the builtin's JSON schema can omit fields safely.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct TaskBudget {
    /// Maximum wall-clock time for the entire worker, in milliseconds.
    #[serde(default = "default_wall_ms")]
    pub max_wall_ms: u64,
    /// Maximum cumulative input tokens (provider-side).
    #[serde(default = "default_input_tokens")]
    pub max_input_tokens: u64,
    /// Maximum cumulative output tokens (provider-side).
    #[serde(default = "default_output_tokens")]
    pub max_output_tokens: u64,
    /// Maximum total tool invocations.
    #[serde(default = "default_tool_calls")]
    pub max_tool_calls: u32,
}

impl Default for TaskBudget {
    fn default() -> Self {
        Self {
            max_wall_ms: default_wall_ms(),
            max_input_tokens: default_input_tokens(),
            max_output_tokens: default_output_tokens(),
            max_tool_calls: default_tool_calls(),
        }
    }
}

/// JSON input for the `Task` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskInput {
    /// Natural-language goal for the sub-agent.
    pub goal: String,
    /// Allow-listed tools the sub-agent may invoke.
    pub allowed_tools: Vec<String>,
    /// Optional budget override; defaults applied per field if omitted.
    #[serde(default)]
    pub budget: TaskBudget,
    /// Optional model override for this sub-agent (per-agent routing). `None`
    /// ⇒ the worker uses the daemon default model.
    #[serde(default)]
    pub model: Option<String>,
}

/// Actionable inlined view of the worker's [`CompletionReport`].
#[derive(Debug, Clone, Serialize)]
pub struct TaskOutput {
    /// Terminal status, lower-snake-cased: `completed`, `goal_unreachable`,
    /// `budget_exhausted`, or `aborted`.
    pub status: String,
    /// Short human-readable summary mentioning the goal and status.
    pub summary: String,
    /// Hex-encoded 32-byte CAS handles of files the worker touched.
    pub files_touched: Vec<String>,
    /// Goal strings of any follow-up tasks the worker suggested.
    pub follow_ups: Vec<String>,
}

/// Errors surfaced by [`task_tool`].
#[derive(Debug, Error)]
pub enum TaskError {
    /// Spawn / await propagated a swarm-layer error.
    #[error("swarm: {0}")]
    Swarm(#[from] SwarmError),
    /// Reserved for future serialization paths (the inline view today is
    /// infallible, but keeping the variant avoids a breaking change when the
    /// real agent loop arrives).
    #[error("json: {0}")]
    Json(String),
}

/// Spawn a worker for `input.goal` WITHOUT awaiting it, returning its handle.
///
/// The worker begins running immediately on the coordinator's independent pool.
/// This is the spawn half of [`task_tool`]: the agent loop spawns every `Task`
/// in a turn up front, then [`task_await`]s them — so multiple sub-agents run
/// concurrently instead of one-at-a-time. Pair every `task_spawn` with exactly
/// one `task_await`.
///
/// # Errors
/// Returns [`TaskError::Swarm`] if the spawn fails.
pub async fn task_spawn(coord: &Coordinator, input: TaskInput) -> Result<WorkerHandle, TaskError> {
    let spec = WorkerSpec {
        goal: input.goal,
        allowed_tools: input.allowed_tools,
        budget: Budget {
            max_wall_ms: input.budget.max_wall_ms,
            max_input_tokens: input.budget.max_input_tokens,
            max_output_tokens: input.budget.max_output_tokens,
            max_tool_calls: input.budget.max_tool_calls,
        },
        workspace: None,
        parent_actor: origin_plan::ActorId::new(0),
        model: input.model,
    };
    Ok(coord.spawn(spec).await?)
}

/// Await a worker previously spawned by [`task_spawn`] and build its actionable
/// view. `goal` is only used to label the summary.
///
/// # Errors
/// Returns [`TaskError::Swarm`] if `await_completion` fails (e.g. the worker
/// reported `Failed`).
pub async fn task_await(coord: &Coordinator, handle: &WorkerHandle, goal: &str) -> Result<TaskOutput, TaskError> {
    let report = coord.await_completion(handle).await?;
    let status = match report.status {
        ReportStatus::Completed => "completed",
        ReportStatus::GoalUnreachable => "goal_unreachable",
        ReportStatus::BudgetExhausted => "budget_exhausted",
        ReportStatus::Aborted => "aborted",
    };
    Ok(TaskOutput {
        status: status.to_owned(),
        summary: format!("worker for {goal:?} reported {:?}", report.status),
        files_touched: report.files_touched.iter().map(hex::encode).collect(),
        follow_ups: report.follow_ups.into_iter().map(|t| t.goal).collect(),
    })
}

/// Spawn a worker for `input.goal`, await completion, return the actionable view.
///
/// Convenience wrapper over [`task_spawn`] + [`task_await`] for the single-task
/// and test paths; the agent loop splits the two halves to parallelise.
///
/// # Errors
/// Returns [`TaskError::Swarm`] if spawn or `await_completion` fails.
#[allow(clippy::module_name_repetitions)] // `task_tool` in module `task` — matches recall_tool / ask_tool precedent
pub async fn task_tool(coord: &Coordinator, input: TaskInput) -> Result<TaskOutput, TaskError> {
    let goal = input.goal.clone();
    let handle = task_spawn(coord, input).await?;
    task_await(coord, &handle, &goal).await
}

crate::origin_tool! {
    name: "Task",
    description: "Dispatch a sub-agent with a goal, allowed tools, and budget. Returns a structured CompletionReport summary.",
    tier: crate::Tier::RequiresPermission,
    urgency: crate::Urgency::Medium,
    side_effects: crate::SideEffects::Mutating,
    input_schema: r#"{"type":"object","required":["goal","allowed_tools"],"properties":{"goal":{"type":"string"},"allowed_tools":{"type":"array","items":{"type":"string"}},"budget":{"type":"object"},"model":{"type":"string"}}}"#,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}
