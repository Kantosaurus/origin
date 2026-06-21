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
use origin_cas::Store as CasStore;
use origin_keyvault::KeyVault;
use origin_permission::prompt::Prompter;
use origin_provider::Provider;
use origin_swarm::{CompletionReport, McpServerSpec, ReportStatus, Usage, WorkerContext, WorkerFn};
use origin_tools::{DynTool, SandboxProfile, SideEffects, Tier, ToolMeta, Urgency, DEFAULT_TOKEN_BUDGET};
use tokio::sync::RwLock;

use crate::agent::{run_loop, scope_runtime_tools, scope_swarm_collab, LoopOptions, SwarmCollab};
use crate::session::Session;

/// Run the MCP `initialize` handshake, then `tools/list`.
///
/// The MCP wire protocol requires the `initialize` request/response to precede
/// every other method, so a spec-compliant server rejects a bare `tools/list`.
/// Issuing the list without the handshake (the pre-fix behaviour) made the list
/// error out, so the server was skipped and the sub-agent silently ran with zero
/// inline-MCP tools — defeating the whole inline-MCP-per-subagent feature.
///
/// # Errors
/// Forwards the [`ClientError`](origin_mcp::client::ClientError) from either the
/// handshake or the list call.
async fn handshake_and_list(
    client: &origin_mcp::client::McpClient,
) -> Result<origin_mcp::client::ListToolsResult, origin_mcp::client::ClientError> {
    client.initialize().await?;
    client.list_tools().await
}

/// Spin up each declared inline-MCP server and wrap its tools as [`DynTool`]s
/// namespaced `mcp__<server>__<tool>` (gap 9b: inline-MCP-per-subagent). Returns
/// the worker-scoped runtime registry plus the namespaced tool names to add to
/// the worker's allow-list. Best-effort: a server that fails to spawn, handshake,
/// or list its tools is logged and skipped (the worker still runs without it).
///
/// Each proxy is wired with (a) the server's declared JSON Schema, so model args
/// are validated before each `tools/call`, and (b) the daemon CAS store, so a
/// large tool result is offloaded to content-addressed storage instead of being
/// inlined into the transcript. HTTP servers get a best-effort OAuth bearer from
/// the vault before the handshake so authenticated endpoints are reachable.
async fn build_runtime_tools(
    specs: &[McpServerSpec],
    cas: &Arc<CasStore>,
    vault: &KeyVault,
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
            let http = Arc::new(origin_mcp::transport_http::HttpTransport::new(url.clone(), None));
            // Best-effort OAuth: an authenticated HTTP MCP server needs its bearer
            // set BEFORE the initialize/list handshake. The token lives in the vault
            // under (provider = "mcp-<server>", account = "default/oauth"); a missing
            // secret just means the server is public, so we proceed unauthenticated.
            if let Err(e) =
                origin_mcp::oauth::attach_bearer(vault, &format!("mcp-{}", spec.name), "default", &http).await
            {
                tracing::debug!(server = %spec.name, error = %e, "inline-MCP: no OAuth bearer attached (proceeding unauthenticated)");
            }
            http as Arc<dyn origin_mcp::transport::Transport>
        } else {
            tracing::warn!(server = %spec.name, "inline-MCP: server declares neither command nor url; skipping");
            continue;
        };
        let client = Arc::new(origin_mcp::client::McpClient::new(transport));
        let listed = match handshake_and_list(&client).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(server = %spec.name, error = %e, "inline-MCP: initialize/tools-list failed; skipping");
                continue;
            }
        };
        // One schema cache per server, shared by all of its tool proxies.
        let schemas = Arc::new(origin_mcp::schema::SchemaCache::new());
        for tool in listed.tools {
            // Register the server's declared schema under the REMOTE tool name (the
            // proxy validates against `remote_name`). Best-effort: a schema that
            // doesn't compile is left unregistered, and `validate` treats an unknown
            // tool as pass-through, so a non-conforming server still works.
            if let Err(e) = schemas.register(&tool.name, &tool.input_schema) {
                tracing::debug!(server = %spec.name, tool = %tool.name, error = %e, "inline-MCP: schema register skipped (pass-through)");
            }
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
            // Wire schema validation (validate model args pre-call) and CAS hand-off
            // (offload large results) — both previously left at their `None` defaults,
            // so neither protection was active for any shipped inline-MCP tool.
            let proxy = origin_mcp::proxy::McpToolProxy::new(Arc::clone(&client), meta, tool.name.clone())
                .with_schemas(Arc::clone(&schemas))
                .with_cas(Arc::clone(cas), 16 * 1024);
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
pub fn real_worker(
    active: ActiveProvider,
    cas: Arc<CasStore>,
    vault: KeyVault,
    plan: origin_planner::Plan,
) -> WorkerFn {
    Arc::new(move |ctx: WorkerContext| {
        let active = Arc::clone(&active);
        let cas = Arc::clone(&cas);
        let vault = vault.clone();
        // The daemon's process-wide cache-band `Plan` (shared with the provider
        // wire-encoder). Each worker forks a session-isolated view sharing the
        // content-addressed handle bands (sub-agent prefix-cache inheritance).
        let plan = plan.clone();
        // Heap-box the (large) worker future to keep the closure's stack small.
        Box::pin(async move { Box::pin(run_worker(active, cas, vault, plan, ctx)).await })
    })
}

/// Drive one worker to completion. Always returns `Ok` with a report — a failed
/// `run_loop` becomes a `GoalUnreachable` report rather than a swarm error, so a
/// sub-agent failure surfaces to the parent as data, not a torn-down turn.
/// Wire a worker's two live-progress relays onto `opts` so a focused agent's
/// transcript streams to the TUI.
///
/// Relay (a) maps the loop's `event_tx` UI events — tool starts (`ToolStarted`,
/// which also drives the panel's current-tool line) and tool results
/// (`ToolResult`) — onto the progress channel. Relay (b) maps the streamed
/// assistant text: token deltas flow ONLY through the streaming ring's relay
/// subscriber (`relay_tx`), never `event_tx`, so a relay subscriber is required
/// to surface the sub-agent's prose as `AssistantText` (this is why a worker
/// with a progress consumer streams). Both feed the same `progress` channel; the
/// relays are detached Realtime tasks that end when the loop drops `opts`.
fn wire_worker_progress(opts: &mut LoopOptions, progress: origin_swarm::WorkerProgressTx) {
    // (a) tool activity + results, off the loop's UI event channel.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::protocol::StreamEvent>(256);
    opts.event_tx = Some(tx);
    let progress_tools = progress.clone();
    drop(origin_runtime::spawn_in(
        origin_runtime::TaskClass::Realtime,
        async move {
            use crate::protocol::StreamEvent as Ev;
            use origin_swarm::WorkerProgress as P;
            while let Some(ev) = rx.recv().await {
                let mapped = match ev {
                    Ev::ToolActivity { tool, .. } => Some(P::ToolStarted(tool)),
                    Ev::ToolResult {
                        tool, ok, preview, ..
                    } => Some(P::ToolResult { tool, ok, preview }),
                    _ => None,
                };
                if let Some(p) = mapped {
                    // Parent gone ⇒ send fails ⇒ stop forwarding.
                    if progress_tools.send(p).is_err() {
                        break;
                    }
                }
            }
        },
    ));
    // (b) assistant prose, off the streaming token ring: `run_streaming_turn`
    // hands one subscriber per turn through `relay_tx`, which `relay_to_progress`
    // drains into coalesced `AssistantText`.
    let (tx_sub, mut rx_sub) = tokio::sync::mpsc::channel::<origin_stream::Subscriber>(1);
    opts.relay_tx = Some(tx_sub);
    drop(origin_runtime::spawn_in(
        origin_runtime::TaskClass::Realtime,
        async move {
            while let Some(sub) = rx_sub.recv().await {
                if crate::stream_relay::relay_to_progress(sub, &progress).await.is_err() {
                    break;
                }
            }
        },
    ));
}

async fn run_worker(
    active: ActiveProvider,
    cas: Arc<CasStore>,
    vault: KeyVault,
    plan: origin_planner::Plan,
    mut ctx: WorkerContext,
) -> Result<CompletionReport, origin_swarm::SwarmError> {
    let provider = active.read().await.clone();
    // Per-agent routing (openclaude): use the worker's explicit model override
    // when set, else the daemon default.
    let model = ctx
        .spec
        .model
        .clone()
        .unwrap_or_else(crate::model_default::configured_default_model);
    let mut session = Session::new(provider.name(), &model);

    // gap 9b: spin up this sub-agent's declared inline-MCP servers and expose
    // their tools to the worker for the run. Empty specs ⇒ empty map ⇒ no MCP
    // (byte-identical default).
    let (mcp_tools, mcp_tool_names) = build_runtime_tools(&ctx.spec.mcp_servers, &cas, &vault).await;

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
    // A transcript consumer (the interactive TUI watching this agent) wants the
    // sub-agent's assistant text live, so it must STREAM (token deltas only flow
    // on the streaming path). Without a consumer the worker stays non-streaming
    // (simpler accounting, byte-identical to before).
    let want_transcript = ctx.progress.is_some();
    let mut opts = LoopOptions {
        max_turns,
        // Stream only when a transcript consumer is attached (see above).
        streaming_disabled: !want_transcript,
        // #13 / N7.1+P9.7 sub-agent prefix-cache inheritance — WIRED via the live
        // handle-band `Plan`. `fork_shared_handle_bands` hands the worker a `Plan`
        // that SHARES the daemon's process-wide, content-addressed `handle_bands`
        // map (the same one the Anthropic wire-encoder reads), so the child
        // (a) inherits every CAS-handle band the parent already assigned —
        // identical content the parent marked `Sticky` is reused as a cacheable
        // `Reference` instead of re-inlined — and (b) its own registrations stay
        // visible to that encoder. Per-session marker state is isolated, so
        // concurrent siblings never clobber one another. (This supersedes the
        // old `SectionId`→band `PrefixLedger` inheritance seam, which the live
        // wire path never consumed and which has now been removed.)
        plan: Some(plan.fork_shared_handle_bands()),
        // Match the orchestrator's reasoning effort so the workers that do the
        // actual editing are not silently downgraded (anthropic ⇒ Ultracode).
        effort: worker_effort(provider.name()),
        ..Default::default()
    };

    // When the spawner asked for progress, wire this worker's live output onto
    // the progress channel (tools + streamed assistant prose) so the TUI can
    // focus the agent and watch its full conversation. `None` ⇒ byte-identical.
    if let Some(progress) = ctx.progress.take() {
        wire_worker_progress(&mut opts, progress);
    }

    let goal = ctx.spec.goal.clone();
    // Real-time swarm collaboration (WS-L, jcode L238). When the coordinator
    // handed this worker a collab handle (only when `ORIGIN_SWARM_COLLAB` was
    // set at coordinator construction), install it as the daemon's per-worker
    // task-local for the duration of `run_loop`: the per-tool hook then records
    // this worker's reads/edits and pushes a file-shift notice into the mailbox
    // of every sibling that had read a path this worker just edited. When the
    // handle is absent (the default) we call the bare `run_loop`, so the loop
    // sees an unset task-local and behaves exactly as before — byte-identical.
    let run = async { run_loop(&mut session, &goal, provider.as_ref(), &prompter, &opts).await };
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
            detail: None,
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
                detail: Some(format!("run_loop failed: {e}")),
            }
        }
    };
    Ok(report)
}

/// Reasoning effort for a sub-agent turn. Workers do the actual editing, so a
/// silent downgrade tanks their accuracy: match the orchestrator's default
/// (anthropic ⇒ Ultracode = max reasoning + always-on swarm). Mirrors the main
/// loop's `resolve_turn_effort` for the no-explicit-effort case; non-anthropic
/// providers stay `None` (wire byte-identical).
fn worker_effort(provider_name: &str) -> Option<origin_provider::ReasoningEffort> {
    (provider_name == "anthropic").then_some(origin_provider::ReasoningEffort::Ultracode)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{worker_effort, AllowList};
    use origin_permission::prompt::Prompter;
    use origin_provider::ReasoningEffort;
    use origin_tools::{registry_iter, ToolMeta};

    #[test]
    fn sub_agents_inherit_ultracode_effort_on_anthropic() {
        assert_eq!(
            worker_effort("anthropic"),
            Some(ReasoningEffort::Ultracode),
            "workers must not be silently downgraded below the orchestrator's effort"
        );
        assert_eq!(worker_effort("openai"), None, "non-anthropic stays wire byte-identical");
    }

    fn meta(name: &str) -> &'static ToolMeta {
        registry_iter()
            .find(|m| m.name == name)
            .expect("tool must be registered")
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

    /// Regression: inline-MCP must run the `initialize` handshake before
    /// `tools/list`, or a spec-compliant server rejects the list and the
    /// sub-agent silently gets zero MCP tools (gap 9b non-functional).
    mod mcp_lifecycle {
        use async_trait::async_trait;
        use origin_mcp::client::McpClient;
        use origin_mcp::transport::{Transport, TransportError};
        use serde_json::{json, Value};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        /// A spec-compliant MCP server that rejects every request issued before
        /// the `initialize` handshake completes.
        struct StrictServer {
            initialized: AtomicBool,
        }

        #[async_trait]
        impl Transport for StrictServer {
            async fn round_trip(&self, request_json: &str) -> Result<Value, TransportError> {
                let req: Value = serde_json::from_str(request_json).map_err(TransportError::Serde)?;
                let id = req.get("id").cloned().unwrap_or(Value::Null);
                match req.get("method").and_then(Value::as_str).unwrap_or("") {
                    "initialize" => {
                        self.initialized.store(true, Ordering::SeqCst);
                        Ok(json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2024-11-05"}}))
                    }
                    "tools/list" if self.initialized.load(Ordering::SeqCst) => Ok(json!({
                        "jsonrpc":"2.0","id":id,
                        "result":{"tools":[{"name":"echo","description":"echo","inputSchema":{"type":"object"}}]}
                    })),
                    // Pre-init request → JSON-RPC error, mirroring a real server.
                    _ => Ok(json!({
                        "jsonrpc":"2.0","id":id,
                        "error":{"code":-32002,"message":"Received request before initialization was complete"}
                    })),
                }
            }
        }

        #[tokio::test]
        async fn handshake_precedes_tools_list() {
            let transport: Arc<dyn Transport> = Arc::new(StrictServer {
                initialized: AtomicBool::new(false),
            });
            let client = McpClient::new(transport);
            // With the handshake, the strict server answers the list.
            let listed = super::super::handshake_and_list(&client)
                .await
                .expect("initialize+list must succeed against a spec-compliant server");
            assert_eq!(listed.tools.len(), 1);
            assert_eq!(listed.tools[0].name, "echo");
        }

        #[tokio::test]
        async fn bare_list_without_handshake_is_rejected() {
            // Control: the pre-fix behaviour (bare `tools/list`) errors out, which
            // is exactly why sub-agents silently ran with zero inline-MCP tools.
            let transport: Arc<dyn Transport> = Arc::new(StrictServer {
                initialized: AtomicBool::new(false),
            });
            let client = McpClient::new(transport);
            assert!(client.list_tools().await.is_err());
        }
    }
}
