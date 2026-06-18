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

// Swarm sub-agents run with UNLIMITED budgets by default: a sub-agent runs to
// completion exactly like the main loop (which itself has no turn cap — see the
// `max_turns: u32::MAX` change). `u*::MAX` is the "no cap" sentinel; the caller
// may still pass an explicit, smaller `budget` per `Task` to bound a worker.
const fn default_wall_ms() -> u64 {
    u64::MAX
}
const fn default_input_tokens() -> u64 {
    u64::MAX
}
const fn default_output_tokens() -> u64 {
    u64::MAX
}
const fn default_tool_calls() -> u32 {
    u32::MAX
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
    /// Inline MCP servers for this sub-agent (gap 9b). The model copies these
    /// from the `<origin-subagents>` block when delegating to a declarative
    /// sub-agent that declares `mcp:` servers. Empty ⇒ no MCP (default).
    #[serde(default)]
    pub mcp_servers: Vec<origin_swarm::McpServerSpec>,
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
    Ok(coord.spawn(worker_spec_from(input)).await?)
}

/// Like [`task_spawn`] but attaches a live progress sink.
///
/// The spawner is notified (via an [`origin_swarm::WorkerProgressTx`]) each time
/// the sub-agent starts a tool — the daemon uses this to drive the TUI's
/// per-agent swarm panel (current-tool line).
///
/// # Errors
/// Returns [`TaskError::Swarm`] if the spawn fails.
pub async fn task_spawn_with_progress(
    coord: &Coordinator,
    input: TaskInput,
    progress: Option<origin_swarm::WorkerProgressTx>,
) -> Result<WorkerHandle, TaskError> {
    Ok(coord
        .spawn_with_progress(worker_spec_from(input), progress)
        .await?)
}

/// Build the [`WorkerSpec`] a `Task` launches from its JSON input.
fn worker_spec_from(input: TaskInput) -> WorkerSpec {
    WorkerSpec {
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
        mcp_servers: input.mcp_servers,
    }
}

/// Await a worker previously spawned by [`task_spawn`] and build its actionable
/// view. `goal` is only used to label the summary.
///
/// # Errors
/// Returns [`TaskError::Swarm`] if `await_completion` fails (e.g. the worker
/// reported `Failed`).
pub async fn task_await(
    coord: &Coordinator,
    handle: &WorkerHandle,
    goal: &str,
) -> Result<TaskOutput, TaskError> {
    let report = coord.await_completion(handle).await?;
    let status = match report.status {
        ReportStatus::Completed => "completed",
        ReportStatus::GoalUnreachable => "goal_unreachable",
        ReportStatus::BudgetExhausted => "budget_exhausted",
        ReportStatus::Aborted => "aborted",
    };
    // Surface the worker's masked failure reason (e.g. an unservable-model
    // provider rejection) to the parent agent so it sees WHY the sub-agent
    // failed, not just the `goal_unreachable` status.
    let summary = match &report.detail {
        Some(d) => format!("worker for {goal:?} reported {:?}: {d}", report.status),
        None => format!("worker for {goal:?} reported {:?}", report.status),
    };
    Ok(TaskOutput {
        status: status.to_owned(),
        summary,
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

#[cfg(test)]
mod budget_tests {
    use super::TaskBudget;

    #[test]
    fn default_budget_is_unlimited() {
        let b = TaskBudget::default();
        assert_eq!(b.max_wall_ms, u64::MAX);
        assert_eq!(b.max_input_tokens, u64::MAX);
        assert_eq!(b.max_output_tokens, u64::MAX);
        assert_eq!(b.max_tool_calls, u32::MAX, "no tool-call cap on a default swarm worker");
    }

    #[test]
    fn omitted_budget_fields_deserialize_to_unlimited() {
        // The model may send `"budget": {}` (or omit fields); each missing field
        // must fall back to the unlimited sentinel, not a small cap.
        let b: TaskBudget = serde_json::from_str("{}").expect("empty budget");
        assert_eq!(b.max_tool_calls, u32::MAX);
        assert_eq!(b.max_input_tokens, u64::MAX);
    }
}

crate::origin_tool! {
    name: "Task",
    description: "Dispatch a sub-agent (swarm worker) with a goal, allowed tools, and budget; it runs concurrently and returns a structured CompletionReport summary. Prefer this to parallelize independent units of work — spawn several at once. No permission prompt is required; the sub-agent is confined to the `allowed_tools` you grant it.",
    // AutoAllowed: spawning a swarm worker never prompts. The child is confined
    // to the `allowed_tools` allow-list the parent grants, and any governance /
    // conseca deny overlay still applies — so removing the parent-side prompt is
    // safe while making swarm delegation friction-free (always-on swarm).
    tier: crate::Tier::AutoAllowed,
    urgency: crate::Urgency::Medium,
    side_effects: crate::SideEffects::Mutating,
    input_schema: r#"{"type":"object","required":["goal","allowed_tools"],"properties":{"goal":{"type":"string"},"allowed_tools":{"type":"array","items":{"type":"string"}},"budget":{"type":"object"},"model":{"type":"string"},"mcp_servers":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"command":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"url":{"type":"string"}},"required":["name"]}}}}"#,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    // hot: the full schema is always advertised to the model (no ToolSearch
    // round-trip first). Task being deferred was why swarm was never invoked.
    hot: true,
}
