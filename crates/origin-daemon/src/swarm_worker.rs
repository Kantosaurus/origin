// SPDX-License-Identifier: Apache-2.0
//! Real swarm worker: runs the agent loop for a child sub-agent.
//!
//! The `Task` tool dispatches a goal to [`origin_swarm::Coordinator`], which
//! until now ran a [`default_noop_worker`](origin_swarm::Coordinator) — it just
//! returned `Completed` without doing anything. This module provides the **real**
//! worker that [`Coordinator::set_default_worker`](origin_swarm::Coordinator::set_default_worker)
//! installs at daemon startup: it builds a fresh [`Session`], narrows the tool
//! set to the worker's `allowed_tools` (minus `Task`, to forbid recursion), and
//! drives [`run_loop`](crate::agent::run_loop) against a snapshot of the active
//! provider, mapping the [`LoopSummary`](crate::agent::LoopSummary) into a
//! [`CompletionReport`](origin_swarm::CompletionReport).
//!
//! **Deadlock safety:** the coordinator spawns worker bodies in
//! [`TaskClass::Swarm`](origin_runtime::TaskClass) (an independent, RAM-admission
//! permit pool, not gated on Critical-idle), so a parent agent — which holds a
//! `Critical` permit while it awaits the child — never contends with the child
//! for the same pool. Combined with stripping `Task` from the child's tools, this
//! prevents the parent↔child circular-wait the `Critical`-on-`Critical` design
//! would cause.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use origin_permission::prompt::Prompter;
use origin_provider::Provider;
use origin_swarm::{CompletionReport, McpServerSpec, ReportStatus, Usage, WorkerContext, WorkerFn};
use origin_tools::{
    DynTool, SandboxProfile, SideEffects, Tier, ToolMeta, Urgency, DEFAULT_TOKEN_BUDGET,
};
use tokio::sync::RwLock;

use crate::agent::{run_loop, scope_runtime_tools, scope_swarm_collab, LoopOptions, SwarmCollab};
use crate::session::Session;

/// Spin up each declared inline-MCP server and wrap its tools as [`DynTool`]s
/// namespaced `mcp__<server>__<tool>` (gap 9b: inline-MCP-per-subagent). Returns
/// the worker-scoped runtime registry plus the namespaced tool names to add to
/// the worker's allow-list. Best-effort: a server that fails to spawn or list its
/// tools is logged and skipped (the worker still runs without it).
async fn build_runtime_tools(
    specs: &[McpServerSpec],
) -> (HashMap<String, Arc<dyn DynTool>>, Vec<String>) {
    let mut map: HashMap<String, Arc<dyn DynTool>> = HashMap::new();
    let mut names: Vec<String> = Vec::new();
    for spec in specs {
        let transport: Arc<dyn origin_mcp::transport::Transport> = if let Some(cmd) = &spec.command {
            match origin_mcp::transport_stdio::StdioTransport::spawn(cmd, &spec.args) {
                Ok(t) => Arc::new(t),
                Err(e) => {
                    tracing::warn!(server = %spec.name, error = %e, "inline-MCP: stdio spawn failed; skipping");
                    continue;
                }
            }
        } else if let Some(url) = &spec.url {
            Arc::new(origin_mcp::transport_http::HttpTransport::new(url.clone(), None))
        } else {
            tracing::warn!(server = %spec.name, "inline-MCP: server declares neither command nor url; skipping");
            continue;
        };
        let client = Arc::new(origin_mcp::client::McpClient::new(transport));
        let listed = match client.list_tools().await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(server = %spec.name, error = %e, "inline-MCP: tools/list failed; skipping");
                continue;
            }
        };
        for tool in listed.tools {
            let full = format!("mcp__{}__{}", spec.name, tool.name);
            // ToolMeta requires `&'static` strings; leak the per-tool metadata
            // (small, process-lifetime, mirroring the static inventory registry).
            let name: &'static str = Box::leak(full.clone().into_boxed_str());
            let description: &'static str = Box::leak(tool.description.into_boxed_str());
            let schema = serde_json::to_string(&tool.input_schema)
                .unwrap_or_else(|_| r#"{"type":"object"}"#.to_string());
            let input_schema: &'static str = Box::leak(schema.into_boxed_str());
            let meta = ToolMeta {
                name,
                description,
                tier: Tier::RequiresPermission,
                urgency: Urgency::Medium,
                side_effects: SideEffects::Mutating,
                input_schema,
                sandbox_profile: SandboxProfile::Inherit,
                token_budget: DEFAULT_TOKEN_BUDGET,
                hot: false,
            };
            let proxy =
                origin_mcp::proxy::McpToolProxy::new(Arc::clone(&client), meta, tool.name.clone());
            map.insert(full.clone(), Arc::new(proxy) as Arc<dyn DynTool>);
            names.push(full);
        }
    }
    (map, names)
}

/// The daemon's live provider handle (swappable via `/account`). The worker
/// snapshots it at spawn time so a mid-flight switch is respected.
type ActiveProvider = Arc<RwLock<Arc<dyn Provider>>>;

/// Default max turns when a worker budget specifies no tool-call cap.
const DEFAULT_WORKER_TURNS: u32 = 32;

/// Permission prompter that allows only an explicit tool allow-list. Tools in
/// the `AutoAllowed` tier never reach the prompter (they are inherently safe,
/// read-only builtins); permission-gated tools (Edit/Write/Bash/…) are denied
/// unless named in the worker's `allowed_tools`.
struct AllowList {
    set: globset::GlobSet,
}

impl AllowList {
    /// Build from the worker's allow-list patterns, EXCLUDING `Task` (a child may
    /// not spawn its own children). Each entry is treated as a glob: a plain name
    /// like `Read` matches only itself, while `mcp__github__*`, `graph_*`, or `*`
    /// match a whole family. A pattern that fails to compile as a glob falls back
    /// to a literal exact-match, so a malformed entry can never *widen* access.
    fn from_patterns(patterns: &[String]) -> Self {
        let mut builder = globset::GlobSetBuilder::new();
        for p in patterns {
            if p == "Task" {
                continue;
            }
            match globset::GlobBuilder::new(p).literal_separator(false).build() {
                Ok(g) => {
                    builder.add(g);
                }
                Err(_) => {
                    if let Ok(g) = globset::Glob::new(&globset::escape(p)) {
                        builder.add(g);
                    }
                }
            }
        }
        Self {
            set: builder.build().unwrap_or_else(|_| globset::GlobSet::empty()),
        }
    }
}

#[async_trait]
impl Prompter for AllowList {
    async fn ask(&self, meta: &ToolMeta, _args_preview: &str) -> bool {
        // `Task` is never delegable to a child (no recursion), regardless of
        // patterns; otherwise glob-match the tool name against the allow-list.
        meta.name != "Task" && self.set.is_match(meta.name)
    }
}

/// Build the real worker closure, capturing the daemon's active-provider handle.
///
/// Installed once at startup via `Coordinator::set_default_worker`. Each spawned
/// worker snapshots the provider, runs a bounded agent loop for its goal, and
/// returns a structured report.
#[must_use]
pub fn real_worker(active: ActiveProvider) -> WorkerFn {
    Arc::new(move |ctx: WorkerContext| {
        let active = Arc::clone(&active);
        // Heap-box the (large) worker future to keep the closure's stack small.
        Box::pin(async move { Box::pin(run_worker(active, ctx)).await })
    })
}

/// Drive one worker to completion. Always returns `Ok` with a report — a failed
/// `run_loop` becomes a `GoalUnreachable` report rather than a swarm error, so a
/// sub-agent failure surfaces to the parent as data, not a torn-down turn.
async fn run_worker(
    active: ActiveProvider,
    ctx: WorkerContext,
) -> Result<CompletionReport, origin_swarm::SwarmError> {
    let provider = active.read().await.clone();
    // Per-agent routing (openclaude): use the worker's explicit model override
    // when set, else the daemon default.
    let model = ctx.spec.model.clone().unwrap_or_else(|| {
        std::env::var("ORIGIN_MODEL").unwrap_or_else(|_| "claude-opus-4-7".to_string())
    });
    let mut session = Session::new(provider.name(), &model);

    // gap 9b: spin up this sub-agent's declared inline-MCP servers and expose
    // their tools to the worker for the run. Empty specs ⇒ empty map ⇒ no MCP
    // (byte-identical default).
    let (mcp_tools, mcp_tool_names) = build_runtime_tools(&ctx.spec.mcp_servers).await;

    // Narrow the child's tools to its allow-list (glob patterns supported) plus
    // its inline-MCP tool names, and never `Task` (a child that could spawn its
    // own children would re-enter the Swarm pool and risk the same circular wait
    // this design avoids).
    let mut allow_patterns = ctx.spec.allowed_tools.clone();
    allow_patterns.extend(mcp_tool_names);
    let prompter = AllowList::from_patterns(&allow_patterns);

    let max_turns = if ctx.budget.max_tool_calls == 0 {
        DEFAULT_WORKER_TURNS
    } else {
        ctx.budget.max_tool_calls
    };
    let opts = LoopOptions {
        max_turns,
        // Sub-agents have no client to stream to; the non-streaming path returns
        // the same assistant text and is simpler to account for.
        streaming_disabled: true,
        ..Default::default()
    };

    let goal = ctx.spec.goal.clone();
    // Real-time swarm collaboration (WS-L, jcode L238). When the coordinator
    // handed this worker a collab handle (only when `ORIGIN_SWARM_COLLAB` was
    // set at coordinator construction), install it as the daemon's per-worker
    // task-local for the duration of `run_loop`: the per-tool hook then records
    // this worker's reads/edits and pushes a file-shift notice into the mailbox
    // of every sibling that had read a path this worker just edited. When the
    // handle is absent (the default) we call the bare `run_loop`, so the loop
    // sees an unset task-local and behaves exactly as before — byte-identical.
    let run = async {
        run_loop(&mut session, &goal, provider.as_ref(), &prompter, &opts).await
    };
    // gap 9b: install the worker's inline-MCP runtime registry for the duration
    // of its loop so the dispatch path can resolve + invoke those tools (empty
    // map ⇒ no effect). Heap-box the (large) composed future.
    let run = Box::pin(scope_runtime_tools(Arc::new(mcp_tools), run));
    let loop_result = match ctx.collab.clone() {
        Some(wc) => {
            let collab = SwarmCollab {
                worker_id: wc.worker_id,
                registry: wc.registry,
                mailboxes: Some(wc.mailboxes),
            };
            scope_swarm_collab(collab, run).await
        }
        None => run.await,
    };
    let report = match loop_result {
        Ok(summary) => CompletionReport {
            goal,
            status: ReportStatus::Completed,
            plan_updates: Vec::new(),
            files_touched: Vec::new(),
            decisions: Vec::new(),
            follow_ups: Vec::new(),
            transcript_handle: [0; 32],
            usage: Usage {
                input_tokens: summary.input_tokens,
                output_tokens: summary.output_tokens,
                tool_calls: summary.turns,
            },
        },
        Err(e) => {
            tracing::warn!(error = %e, goal = %goal, "swarm worker: run_loop failed");
            CompletionReport {
                goal,
                status: ReportStatus::GoalUnreachable,
                plan_updates: Vec::new(),
                files_touched: Vec::new(),
                decisions: Vec::new(),
                follow_ups: Vec::new(),
                transcript_handle: [0; 32],
                usage: Usage::default(),
            }
        }
    };
    Ok(report)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::AllowList;
    use origin_permission::prompt::Prompter;
    use origin_tools::{registry_iter, ToolMeta};

    fn meta(name: &str) -> &'static ToolMeta {
        registry_iter().find(|m| m.name == name).expect("tool must be registered")
    }

    #[tokio::test]
    async fn exact_name_matches_only_itself() {
        let al = AllowList::from_patterns(&["Read".to_string()]);
        assert!(al.ask(meta("Read"), "").await);
        assert!(!al.ask(meta("Write"), "").await);
    }

    #[tokio::test]
    async fn star_matches_everything_except_task() {
        let al = AllowList::from_patterns(&["*".to_string()]);
        assert!(al.ask(meta("Read"), "").await);
        assert!(al.ask(meta("Write"), "").await);
        assert!(al.ask(meta("Bash"), "").await);
        // `Task` is always denied (no recursion), even under `*`.
        assert!(!al.ask(meta("Task"), "").await);
    }

    #[tokio::test]
    async fn prefix_glob_matches_namespace_family() {
        let al = AllowList::from_patterns(&["graph_*".to_string()]);
        assert!(al.ask(meta("graph_query"), "").await);
        assert!(al.ask(meta("graph_explain"), "").await);
        assert!(!al.ask(meta("Read"), "").await);
    }

    #[tokio::test]
    async fn empty_allow_list_denies_all() {
        let al = AllowList::from_patterns(&[]);
        assert!(!al.ask(meta("Read"), "").await);
    }

    #[tokio::test]
    async fn explicit_task_pattern_is_still_denied() {
        let al = AllowList::from_patterns(&["Task".to_string(), "Read".to_string()]);
        assert!(!al.ask(meta("Task"), "").await);
        assert!(al.ask(meta("Read"), "").await);
    }
}
