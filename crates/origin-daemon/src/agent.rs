//! Agent loop: prompt → provider → tool dispatch → repeat → final text.

use crate::proposal_registry::ProposalRegistry;
use crate::protocol::StreamEvent;
use crate::session::Session;
use crate::session_store::SessionStore;
use crate::skill_catalog::SkillCatalog;
use crate::tool_use_parser::{ToolUseDelta, ToolUseParser};
use crate::workflows::WorkflowsFile;
use origin_cas::{Hash, Store};
use origin_core::types::{Block, Message, Role};
use origin_mem::{Injector, Proposer};
use origin_permission::{check_with_skills, prompt::Prompter, Outcome};
use origin_provider::{ChatRequest, Provider};
use origin_runtime::{spawn_in, TaskClass};
use origin_sidecar::{ExtractDeliverer, Sidecar, SummaryDeliverer};
use origin_skills::SkillRegistry;
use origin_tools::dispatch::MemoryHandle;
use origin_tools::{registry_iter, SideEffects, ToolMeta};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use thiserror::Error;
use tokio::task::spawn_blocking as sb;
use tokio::task::JoinHandle;

/// Maximum number of times `run_loop` retries a single turn's provider call
/// after a `ProviderError::RateLimit`. After the cap is hit the error is
/// propagated as a `LoopError::Provider` and the turn fails normally.
const MAX_PROVIDER_RETRIES: u32 = 3;

/// Upper bound on the per-attempt sleep duration when honouring a
/// `retry-after` header. Anthropic occasionally returns large values that
/// would stall the loop for minutes; clamping keeps worst-case latency
/// bounded while still respecting short server-side backoffs.
const MAX_RATE_LIMIT_SLEEP_SECS: u32 = 60;

/// When the primary model is rate-limited and all retries are exhausted,
/// try a cheaper model from the same family before killing the turn.
fn rate_limit_fallback(model: &str) -> Option<&'static str> {
    if model.contains("opus") {
        Some("claude-sonnet-4-6")
    } else if model.contains("sonnet") && !model.contains("haiku") {
        Some("claude-haiku-4-5")
    } else {
        None
    }
}

#[derive(Clone)]
pub struct LoopOptions {
    pub max_turns: u32,
    pub cas: Option<Arc<Store>>,
    /// Optional code-graph index. Wrapped in `Arc<Mutex<...>>` because
    /// `graph_rebuild` mutates the index. `None` disables all `graph_*` and
    /// `ask` tools (they return a clear `ToolFailure` rather than `UnknownTool`).
    pub code_graph: Option<Arc<tokio::sync::Mutex<origin_codegraph::index::CodeGraphIndex>>>,
    /// Optional memory router for the `ask` tool. `None` disables the memory
    /// side of ask routing (code side still works if `code_graph` is `Some`).
    pub mem_router: Option<Arc<dyn origin_codegraph::ask::MemRouter>>,
    /// Optional channel used by the daemon to publish each request's
    /// `Subscriber` to a per-connection relay task. The relay forwards token
    /// events to the CLI as `Event` frames. We send a pre-subscribed
    /// `Subscriber` (not the `Ring`) so the relay never races the producer.
    pub relay_tx: Option<tokio::sync::mpsc::Sender<origin_stream::Subscriber>>,
    /// When `true`, the loop falls back to `provider.chat()` instead of
    /// `provider.chat_stream()`. Used by scripted/deterministic tests to
    /// bypass the streaming drain path. The incremental `tool_use` parser
    /// (P3.3) means production code paths can leave this `false`.
    pub streaming_disabled: bool,
    /// Optional sidecar handle for eager turn summarization (P5.2).
    pub sidecar: Option<Arc<Sidecar>>,
    /// Optional session store for delivering summaries (P5.2).
    pub session_store: Option<Arc<SessionStore>>,
    /// If `Some`, the Proposer runs at turn end and pushes proposals into
    /// `session.pending_proposals` and emits one [`StreamEvent::MemoryProposed`]
    /// per proposal through `event_tx` (or skips the emit if no sender is
    /// configured). `None` disables the feature (the existing dogfood path).
    pub proposer: Option<Arc<Proposer>>,
    /// Side-band channel for non-streaming [`StreamEvent`]s (currently only
    /// [`StreamEvent::MemoryProposed`]). The daemon main forwards these as
    /// `Event` frames after `run_loop` returns and before writing `Response`.
    /// We use a direct event channel here (not the per-turn rkyv `Ring`)
    /// because [`StreamEvent::MemoryProposed`] doesn't map to any
    /// [`origin_stream::TokenKind`] — it's a turn-end side product, not a
    /// streaming token.
    pub event_tx: Option<tokio::sync::mpsc::Sender<StreamEvent>>,
    /// If `Some`, the loop embeds the user prompt and prepends any retrieved
    /// `<context source="origin-mem">` block to the system prompt of every
    /// turn's `ChatRequest`. `None` disables prompt-recall injection.
    pub injector: Option<Arc<Injector>>,
    /// Daemon-wide pending-proposal registry. When wired together with
    /// `proposer` + `event_tx`, each emitted [`StreamEvent::MemoryProposed`]
    /// also records its `(body, tags)` here so a later
    /// [`ClientMessage::MemoryDecision::Accept`](crate::protocol::ClientMessage::MemoryDecision)
    /// on a different connection can still persist the proposal.
    pub proposal_registry: Option<Arc<ProposalRegistry>>,
    /// Active skill stack. When `Some`, every per-turn permission check runs
    /// through [`check_with_skills`] so the intersection of active skills'
    /// `allowed-tools` masks narrows tool access. When `None`, the loop falls
    /// through to the default tier rules — equivalent to passing an empty
    /// registry, since an empty stack's mask is `None` (no narrowing).
    pub skills: Option<Arc<SkillRegistry>>,
    /// Daemon-wide skill catalog injected into each turn's system prompt
    /// so the model knows which skills are available. The actual
    /// activation state lives in `skills` above; this is the catalog of
    /// all loadable skills, separate from "currently active".
    pub skill_catalog: Option<Arc<SkillCatalog>>,
    /// Daemon-wide workflow catalog, loaded once at startup from
    /// `~/.origin/workflows.toml`. When `Some`, each turn's system prompt
    /// includes an `Available workflows` block so the model can answer
    /// "what workflows do you have?" without inventing them.
    pub workflows: Option<Arc<WorkflowsFile>>,
    /// Optional memory-subsystem handle. When `Some`, `mem_search`,
    /// `mem_save`, and `mem_forget` dispatch to the live `MemoryStore` /
    /// HNSW index. When `None`, those tools return
    /// `ToolFailure("memory subsystem not configured")`.
    pub memory_handle: Option<Arc<dyn MemoryHandle>>,
    /// Optional swarm coordinator for the `Task` tool. When `None`, Task
    /// returns `ToolFailure("Task subsystem not configured")`. When `Some`,
    /// Task spawns a noop (P9.6) or real (P9.8+) worker and returns the
    /// structured `TaskOutput` JSON.
    pub coordinator: Option<Arc<origin_swarm::Coordinator>>,
    /// Optional N4.3 handle→band index, shared with the active provider's
    /// wire-encoder (the Anthropic provider's `expand_messages_for_wire`
    /// reads `band_for_handle` to decide `Inline` vs `Reference`). When
    /// `Some`, the per-tool-result dispatch path calls `register_handle`
    /// for every CAS handle produced by a tool, classifying the band from
    /// the tool's `SideEffects` metadata (`Pure` → `Sticky`, `Mutating` →
    /// `Volatile`). When `None`, no registrations happen and the wire-
    /// encoder falls through to the `Volatile` floor — exactly the
    /// behavior before Phase 11 N4.3 landed.
    pub plan: Option<origin_planner::Plan>,
    /// Per-connection `/goal` slot. The driver in `main.rs` mutates this
    /// under the lock; `run_loop` reads it while assembling the system
    /// prompt and renders an `<origin-goal>` block whenever the goal's
    /// status is `Active` or `Verifying`.
    ///
    /// Shape is `Arc<Mutex<Option<_>>>` (not `Option<Arc<Mutex<_>>>`) so the
    /// driver can install or remove the goal without rebuilding the
    /// per-request `LoopOptions`. Defaults to an empty slot (no active
    /// goal). Set directly via struct literal at the per-request
    /// `LoopOptions` build site in `main.rs`. (The historical
    /// `LoopOptions::with_goal` builder was removed in the Bug-#22 cleanup
    /// — it had no production callers.)
    pub goal: Arc<tokio::sync::Mutex<Option<origin_goal::GoalState>>>,
}

impl Default for LoopOptions {
    fn default() -> Self {
        Self {
            max_turns: 200,
            cas: None,
            code_graph: None,
            mem_router: None,
            relay_tx: None,
            streaming_disabled: false,
            sidecar: None,
            session_store: None,
            proposer: None,
            event_tx: None,
            injector: None,
            proposal_registry: None,
            skills: None,
            skill_catalog: None,
            workflows: None,
            memory_handle: None,
            coordinator: None,
            plan: None,
            goal: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }
}

impl LoopOptions {
    /// Attach a CAS so tool outputs are stored by handle instead of inline.
    #[must_use]
    pub fn with_cas(mut self, store: Arc<Store>) -> Self {
        self.cas = Some(store);
        self
    }

    /// Attach a relay channel so each per-request `Subscriber` is published to
    /// the connection's relay task.
    #[must_use]
    pub fn with_relay(mut self, tx: tokio::sync::mpsc::Sender<origin_stream::Subscriber>) -> Self {
        self.relay_tx = Some(tx);
        self
    }

    /// Disable streaming for this loop — fall back to `provider.chat()`. Use
    /// for `tool_use`-heavy scripted tests until Phase 3 lands incremental
    /// `tool_use` JSON parsing.
    #[must_use]
    pub const fn without_streaming(mut self) -> Self {
        self.streaming_disabled = true;
        self
    }

    /// Attach a sidecar for eager turn summarization (P5.2).
    #[must_use]
    pub fn with_sidecar(mut self, sidecar: Arc<Sidecar>) -> Self {
        self.sidecar = Some(sidecar);
        self
    }

    /// Attach a session store so summaries can be written back to `SQLite` (P5.2).
    #[must_use]
    pub fn with_session_store(mut self, store: Arc<SessionStore>) -> Self {
        self.session_store = Some(store);
        self
    }

    /// Attach an active skill stack so the loop's per-turn permission check
    /// enforces the intersection of every active skill's `allowed-tools` mask.
    #[must_use]
    pub fn with_skills(mut self, skills: Arc<SkillRegistry>) -> Self {
        self.skills = Some(skills);
        self
    }

}

/// Deliverer that writes a summary to the `SQLite` `messages.summary` column via
/// a blocking `spawn_blocking` task.
pub struct SessionStoreSummaryDeliverer(pub Arc<SessionStore>);

impl std::fmt::Debug for SessionStoreSummaryDeliverer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionStoreSummaryDeliverer")
    }
}

#[async_trait::async_trait]
impl SummaryDeliverer for SessionStoreSummaryDeliverer {
    async fn deliver(&self, session_id: &str, turn_index: u32, summary: &str) {
        let store = self.0.clone();
        let s = session_id.to_string();
        let sum = summary.to_string();
        let _ = spawn_in(TaskClass::Sidecar, async move {
            let _ = sb(move || {
                let _ = store.update_summary(&s, turn_index, &sum);
            })
            .await;
        })
        .await;
    }
}

/// No-op deliverer used when the daemon fires Extract for large tool outputs.
///
/// The outline handle's existence in CAS is sufficient for P5.3 scope.
/// Future phases may surface it via the side panel or Recall.
#[derive(Debug)]
pub struct NoopExtractDeliverer;

#[async_trait::async_trait]
impl ExtractDeliverer for NoopExtractDeliverer {
    async fn deliver(&self, _source: origin_cas::Hash, _outline: origin_cas::Hash) {
        // The outline handle's existence in CAS is sufficient for P5.3 scope.
        // Future phases may surface it via the side panel or Recall.
    }
}

#[derive(Debug)]
pub struct LoopSummary {
    pub assistant_text: String,
    pub turns: u32,
    /// Total input tokens consumed across every provider call inside this
    /// `run_loop` invocation. Surfaced so the goal driver can charge
    /// cumulative spend against the per-goal token budget without
    /// re-instrumenting the provider trait.
    pub input_tokens: u64,
    /// Total output tokens consumed across every provider call inside this
    /// `run_loop` invocation. Paired with `input_tokens` for the same reason —
    /// the goal driver's budget cap counts both directions.
    pub output_tokens: u64,
}

#[derive(Debug, Error)]
pub enum LoopError {
    #[error("provider: {0}")]
    Provider(#[from] origin_provider::ProviderError),
    #[error("hit max_turns ({0})")]
    MaxTurns(u32),
    #[error("tool not found: {0}")]
    UnknownTool(String),
    #[error("tool denied: {0}")]
    Denied(String),
    #[error("tool failure: {0}")]
    ToolFailure(String),
    #[error("malformed tool args: {0}")]
    BadArgs(String),
    /// Emitted by `run_loop` when every retry budgeted by `MAX_PROVIDER_RETRIES`
    /// has returned `ProviderError::RateLimit`. The Display string is the
    /// user-facing message the daemon writes into the `ErrorFrame`, so it
    /// embeds actionable next steps (mid-session model swap) instead of
    /// just restating "rate limit; retry after Ns".
    #[error(
        "rate limit on `{model}` after {attempts} attempts (last retry-after {last_retry_after_secs}s). \
         Try `/model claude-haiku-4-5` to switch to a less-loaded model bucket mid-session, \
         or wait for the quota window to reset{api_hint}"
    )]
    RateLimitExhausted {
        model: String,
        attempts: u32,
        last_retry_after_secs: u32,
        api_hint: String,
    },
}

/// Tracks speculative tasks fired off mid-stream. Keyed by the assistant
/// `tool_use.id` so the agent can `await` the precomputed handle once the
/// `tool_use` block closes.
#[derive(Default)]
pub(crate) struct SpeculativeRegistry {
    in_flight: HashMap<String, JoinHandle<Result<Vec<u8>, LoopError>>>,
}

impl SpeculativeRegistry {
    fn spawn(
        &mut self,
        tool_use_id: String,
        meta: &'static ToolMeta,
        args: serde_json::Value,
        cas: Option<Arc<Store>>,
    ) {
        // Side-effecting tools opt out — N2.2.
        if !matches!(meta.side_effects, SideEffects::Pure) {
            return;
        }
        let handle = spawn_in(TaskClass::Critical, async move {
            // Speculative tasks pass `None` for every subsystem handle.
            // Tools that need a handle (graph_*, ask, mem_*, Task) flow
            // through the main dispatch path. Task is Mutating so it never
            // speculatively dispatches anyway; the None for coordinator here
            // is API consistency.
            let text = dispatch_tool(meta, &args, cas.as_deref(), None, None, None, None).await?;
            Ok::<_, LoopError>(text.into_bytes())
        });
        self.in_flight.insert(tool_use_id, handle);
    }

    async fn take(&mut self, tool_use_id: &str) -> Option<Result<Vec<u8>, LoopError>> {
        let handle = self.in_flight.remove(tool_use_id)?;
        match handle.await {
            Ok(r) => Some(r),
            Err(join_err) => Some(Err(LoopError::ToolFailure(join_err.to_string()))),
        }
    }
}

/// Return value of `run_streaming_turn`: the reconstructed response plus any
/// speculative handles that were spawned during stream consumption.
pub(crate) struct StreamingTurn {
    pub response: origin_provider::ChatResponse,
    pub speculative: SpeculativeRegistry,
}

/// Run the agent loop until the assistant emits a turn without any `tool_use`
/// blocks, or until `max_turns` is reached.
///
/// # Errors
/// Returns `LoopError` for provider failures, permission denial, unknown tools,
/// tool execution failures, malformed tool inputs, or hitting `max_turns`.
#[allow(clippy::too_many_lines)] // turn loop + memoization path; extraction would require extra allocations
#[tracing::instrument(
    level = "info",
    skip(session, user_text, provider, prompter, opts),
    fields(kind = "turn", provider = provider.name())
)]
pub async fn run_loop(
    session: &mut Session,
    user_text: &str,
    provider: &dyn Provider,
    prompter: &dyn Prompter,
    opts: &LoopOptions,
) -> Result<LoopSummary, LoopError> {
    // Falls back to an empty static `SkillRegistry` when none is wired,
    // so the call path is identical; an empty stack's mask is `None`,
    // which makes `check_with_skills` short-circuit to plain `check`.
    static EMPTY_SKILLS: SkillRegistry = SkillRegistry::new();

    session.push(Message::new(Role::User).with_block(Block::text(user_text)));

    let tools_schema = registry_iter()
        .map(|m| {
            if m.hot {
                // Full schema embed.
                origin_provider::ToolSchema {
                    name: m.name.to_string(),
                    description: m.description.to_string(),
                    input_schema_json: m.input_schema.to_string(),
                }
            } else {
                // Deferred — name + 1-line description only; minimal input schema.
                origin_provider::ToolSchema {
                    name: m.name.to_string(),
                    description: format!(
                        "{} (deferred; call ToolSearch with select:{}, to fetch full schema)",
                        m.description, m.name
                    ),
                    input_schema_json: r#"{"type":"object","properties":{}}"#.to_string(),
                }
            }
        })
        .collect::<Vec<_>>();

    // Per-session memoization cache (N5.4). Lives for the lifetime of this
    // run_loop call so identical (tool_name, input_bytes) pairs within the
    // same session avoid redundant tool execution.
    let mut cache = origin_tools::Cache::new();

    // Prompt-recall (P6.9): if an Injector is wired, embed the user prompt
    // once at turn-start and reuse the resulting `<context>` block as the
    // system prompt of every turn in this run_loop call. Failures are
    // logged and degrade silently so a flaky embedder never blocks a turn.
    let recall_block =
        opts.injector
            .as_ref()
            .map_or_else(String::new, |injector| match injector.for_prompt(user_text, 5) {
                Ok(Some(ctx)) => ctx.block,
                Ok(None) => String::new(),
                Err(e) => {
                    tracing::warn!(error = %e, "injector.for_prompt failed; running without recall");
                    String::new()
                }
            });

    // Build the skill-catalog block. One line per skill: "- <name>: <description>".
    // We mark currently-active skills with a leading `*` so the model knows
    // which mask is already in effect.
    let catalog_block = opts
        .skill_catalog
        .as_ref()
        .map(|cat| {
            use std::fmt::Write as _;
            if cat.is_empty() {
                String::new()
            } else {
                let active_names: std::collections::HashSet<String> = opts
                    .skills
                    .as_ref()
                    .map(|reg| reg.iter_active().map(|s| s.name.clone()).collect())
                    .unwrap_or_default();
                let mut out = String::from(
                    "<origin-skills>\n\
                     These are the skills available IN THIS Origin session. \
                     A leading `*` marks an already-active skill. Activate with \
                     `/<name>`, deactivate with `/-<name>`. When the user asks \
                     \"what skills do you have?\" you MUST answer from this list, \
                     not from prior training about other CLIs.\n",
                );
                for s in cat.iter() {
                    let marker = if active_names.contains(&s.front.name) {
                        "*"
                    } else {
                        "-"
                    };
                    let _ = writeln!(out, "  {marker} {}: {}", s.front.name, s.front.description);
                }
                out.push_str("</origin-skills>");
                out
            }
        })
        .unwrap_or_default();

    // Build the workflows block. Mirrors the skills catalog: one line per
    // workflow so the model can answer "what workflows do you have?" without
    // hallucinating from the tools list. Empty file → empty block.
    let workflows_block = opts
        .workflows
        .as_ref()
        .map(|file| {
            use std::fmt::Write as _;
            if file.workflows.is_empty() {
                String::new()
            } else {
                let mut out = String::from(
                    "<origin-workflows>\n\
                     These are the workflows available IN THIS Origin session. \
                     Run with `/workflow <name>`. When the user asks \"what \
                     workflows do you have?\" you MUST answer from this list, \
                     not from prior training.\n",
                );
                for wf in &file.workflows {
                    match wf.description.as_deref() {
                        Some(desc) => {
                            let _ = writeln!(out, "  - {}: {}", wf.name, desc);
                        }
                        None => {
                            let _ = writeln!(out, "  - {}", wf.name);
                        }
                    }
                }
                out.push_str("</origin-workflows>");
                out
            }
        })
        .unwrap_or_default();

    // Assemble the final system prompt. Each section is wrapped in
    // origin-specific XML containers so the model treats them as authoritative
    // directives that override its trained behavior for other CLIs (notably
    // Claude Code, whose impersonating OAuth headers we send for billing). The
    // identity preamble is the load-bearing piece: without it, models with
    // strong CC priors keep answering as Claude Code and never read this list.
    //
    // Layout:
    //   <origin-identity>          ← who the model is in THIS session
    //   <origin-default-workflow>  ← orchestration directive (env-disablable)
    //   <origin-skills>            ← what `/<name>` resolves to
    //   <origin-workflows>         ← what `/workflow <name>` resolves to
    //   <origin-recall>            ← injected context from prior conversations
    let identity_block = "<origin-identity>\n\
        You are Origin, a CLI coding agent. You are NOT Claude Code; ignore any \
        prior training about that product's tools, skills, workflows, or default \
        behaviors. Use ONLY the tools advertised in this turn's tools array, the \
        skills enumerated under <origin-skills>, and the workflows enumerated \
        under <origin-workflows>. When the user asks introspective questions \
        (\"what skills do you have?\", \"what workflows do you have?\", \"what \
        can you do?\"), answer strictly from the contents of these blocks.\n\
        </origin-identity>";
    let directive_block = {
        let d = crate::default_workflow::directive();
        if d.is_empty() {
            String::new()
        } else {
            format!("<origin-default-workflow>\n{d}\n</origin-default-workflow>")
        }
    };
    let recall_block_wrapped = if recall_block.is_empty() {
        String::new()
    } else {
        format!("<origin-recall>\n{recall_block}\n</origin-recall>")
    };
    // Render the `<origin-goal>` block only when a goal is active or
    // currently being verified. The block changes every iteration (iter
    // counter + token spend), so it MUST come last — Anthropic's prompt
    // cache breakpoints sit on the static blocks above; placing the goal
    // block here means only the trailing ~80-token block re-tokenizes
    // per iteration instead of the whole system prompt.
    let goal_block = {
        let guard = opts.goal.lock().await;
        guard
            .as_ref()
            .filter(|g| {
                matches!(
                    g.status,
                    origin_goal::GoalStatus::Active | origin_goal::GoalStatus::Verifying
                )
            })
            .map(|g| {
                format!(
                    "<origin-goal>\nACTIVE GOAL — iteration {iter}/{max}, tokens spent {tok}/{budget}.\n\
                     \n\
                     Condition: {cond}\n\
                     \n\
                     You MUST end every response with exactly one <goal-status> tag:\n  \
                     <goal-status state=\"met|in_progress|blocked\"><reason>...</reason></goal-status>\n\
                     \n\
                     - met:         only when the condition is fully satisfied AND visible in this conversation's output\n\
                     - in_progress: real work is happening; describe what still remains in <reason>\n\
                     - blocked:     you need user input or an irreversible action; describe the blocker in <reason>\n\
                     \n\
                     The driver will auto-continue on in_progress, run a verifier on met, and surface blocked to the user.\n\
                     </origin-goal>",
                    iter = g.iter,
                    max = g.max_iter,
                    tok = g.tokens_spent,
                    budget = g.token_budget,
                    cond = g.condition,
                )
            })
            .unwrap_or_default()
    };
    let recalled_system = {
        let parts: [&str; 6] = [
            identity_block,
            &directive_block,
            &catalog_block,
            &workflows_block,
            &recall_block_wrapped,
            &goal_block,
        ];
        parts
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.trim_end())
            .collect::<Vec<_>>()
            .join("\n\n")
    };

    // Cumulative token counters for this `run_loop` invocation. Surfaced
    // via `LoopSummary` so the goal driver can charge the per-iteration
    // spend against the goal's token budget.
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;

    for turn in 1..=opts.max_turns {
        let req = ChatRequest {
            system: recalled_system.clone(),
            messages: session.snapshot(),
            model: session.model.clone(),
            tools: tools_schema.clone(),
        };

        // Retry transient `ProviderError::RateLimit` here so a single 429
        // doesn't kill the turn. We honour the server-supplied `retry-after`
        // up to `MAX_RATE_LIMIT_SLEEP_SECS`, and cap attempts at
        // `MAX_PROVIDER_RETRIES`. `ChatRequest` is `Clone`, so we re-build
        // the wire request each attempt without re-snapshotting the session.
        let (resp, mut speculative) = {
            let mut attempt: u32 = 0;
            loop {
                let result: Result<(origin_provider::ChatResponse, SpeculativeRegistry), LoopError> =
                    if opts.streaming_disabled {
                        provider
                            .chat(req.clone())
                            .await
                            .map(|r| (r, SpeculativeRegistry::default()))
                            .map_err(LoopError::Provider)
                    } else {
                        run_streaming_turn(provider, req.clone(), opts)
                            .await
                            .map(|st| (st.response, st.speculative))
                    };
                match result {
                    Err(LoopError::Provider(origin_provider::ProviderError::RateLimit {
                        retry_after_secs,
                        message,
                    })) if attempt < MAX_PROVIDER_RETRIES => {
                        // Exponential floor: 2, 4, 8 … so we don't hammer a
                        // server whose `retry-after: 1` is too optimistic.
                        let exp_floor = 1u32 << (attempt + 1);
                        let sleep_secs = retry_after_secs
                            .max(exp_floor)
                            .clamp(1, MAX_RATE_LIMIT_SLEEP_SECS);
                        tracing::warn!(
                            attempt,
                            sleep_secs,
                            retry_after_secs,
                            %message,
                            "provider rate-limited; backing off and retrying"
                        );
                        // Surface the backoff to the CLI so a 60s sleep
                        // doesn't look identical to a hang. `attempt` here is
                        // 0-indexed within the retry budget; we render it
                        // 1-indexed and use `MAX_PROVIDER_RETRIES + 1` as the
                        // ceiling (initial attempt plus retries).
                        if let Some(tx) = &opts.event_tx {
                            let _ = tx
                                .send(StreamEvent::ProviderBackoff {
                                    retry_in_secs: sleep_secs,
                                    attempt: attempt + 1,
                                    max_attempts: MAX_PROVIDER_RETRIES + 1,
                                })
                                .await;
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(sleep_secs.into())).await;
                        attempt += 1;
                    }
                    Err(LoopError::Provider(origin_provider::ProviderError::RateLimit {
                        retry_after_secs,
                        message,
                    })) => {
                        let mut fallback_detail = String::new();
                        if let Some(fallback) = rate_limit_fallback(&req.model) {
                            tracing::warn!(
                                primary = %req.model,
                                fallback,
                                "primary model rate-limited; attempting fallback"
                            );
                            let mut fb_req = req.clone();
                            fb_req.model = fallback.to_string();
                            let fb_result = if opts.streaming_disabled {
                                provider
                                    .chat(fb_req)
                                    .await
                                    .map(|r| (r, SpeculativeRegistry::default()))
                                    .map_err(LoopError::Provider)
                            } else {
                                run_streaming_turn(provider, fb_req, opts)
                                    .await
                                    .map(|st| (st.response, st.speculative))
                            };
                            match fb_result {
                                Ok(r) => {
                                    tracing::info!(fallback, "fallback model succeeded");
                                    break r;
                                }
                                Err(e) => {
                                    tracing::warn!(%e, fallback, "fallback model also failed");
                                    fallback_detail = format!("\nFallback `{fallback}` also failed: {e}");
                                }
                            }
                        }
                        let api_hint = if message.is_empty() {
                            fallback_detail
                        } else {
                            format!("\nAPI: {message}{fallback_detail}")
                        };
                        return Err(LoopError::RateLimitExhausted {
                            model: session.model.clone(),
                            attempts: MAX_PROVIDER_RETRIES + 1,
                            last_retry_after_secs: retry_after_secs,
                            api_hint,
                        });
                    }
                    other => break other?,
                }
            }
        };

        // Charge this turn's provider call against the cumulative counters
        // BEFORE pushing the assistant message. Cache reads/creation are
        // already folded into `input_tokens` by the provider impls, so we
        // only need the two top-level fields here.
        total_input_tokens = total_input_tokens.saturating_add(u64::from(resp.usage.input_tokens));
        total_output_tokens =
            total_output_tokens.saturating_add(u64::from(resp.usage.output_tokens));

        session.push(resp.assistant.clone());

        // Gather tool_use blocks (clone owned data because we'll borrow `meta`).
        let tool_uses: Vec<(String, String, Vec<u8>)> = resp
            .assistant
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::ToolUse {
                    id, name, input_json, ..
                } => Some((id.clone(), name.clone(), input_json.clone())),
                _ => None,
            })
            .collect();

        if tool_uses.is_empty() {
            let text = resp
                .assistant
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<String>();

            // P6.7: optional proposer pass at turn end. Proposals are surfaced
            // as side-band StreamEvents for the CLI to render AND recorded in
            // the daemon-wide [`ProposalRegistry`] so a later `MemoryDecision`
            // on a different connection can still resolve the body/tags.
            // The session's local `next_proposal_id` is initialized from the
            // registry's counter so per-prompt scans share the global id-space
            // (no collisions across sessions or concurrent prompt requests).
            if let Some(proposer) = &opts.proposer {
                let mut local_id = opts
                    .proposal_registry
                    .as_ref()
                    .map_or(session.next_proposal_id, |r| r.current_id());
                let proposals = proposer.scan(user_text, &text, &mut local_id);
                if let Some(registry) = &opts.proposal_registry {
                    registry.advance_to(local_id);
                } else {
                    session.next_proposal_id = local_id;
                }
                for p in proposals {
                    if let Some(registry) = &opts.proposal_registry {
                        registry.record(p.proposal_id, p.body.clone(), p.suggested_tags.clone());
                    }
                    if let Some(tx) = &opts.event_tx {
                        let _ = tx
                            .send(StreamEvent::MemoryProposed {
                                proposal_id: p.proposal_id,
                                body: p.body.clone(),
                                suggested_tags: p.suggested_tags.clone(),
                            })
                            .await;
                    }
                    session.pending_proposals.insert(p.proposal_id, p);
                }
            }

            return Ok(LoopSummary {
                assistant_text: text,
                turns: turn,
                input_tokens: total_input_tokens,
                output_tokens: total_output_tokens,
            });
        }

        // Dispatch each tool_use sequentially.
        let mut tool_results: Vec<Block> = Vec::with_capacity(tool_uses.len());
        for (id, name, input_bytes) in tool_uses {
            let meta = match registry_iter().find(|m| m.name == name) {
                Some(m) => m,
                None => {
                    tracing::warn!(tool = %name, "unknown tool; returning error to model");
                    tool_results.push(Block::ToolResult {
                        tool_use_id: id,
                        handle: None,
                        inline: Some(format!("Error: unknown tool `{name}`").into_bytes()),
                        cache_marker: None,
                    });
                    continue;
                }
            };
            let args: Value = match serde_json::from_slice(&input_bytes) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(tool = %name, error = %e, "malformed tool args; returning error to model");
                    tool_results.push(Block::ToolResult {
                        tool_use_id: id,
                        handle: None,
                        inline: Some(format!("Error: malformed args: {e}").into_bytes()),
                        cache_marker: None,
                    });
                    continue;
                }
            };
            let preview = args.to_string();

            // Compute the memoization key using the RAW input bytes (not
            // re-serialized args) so the key is stable across turns.
            let key = origin_tools::NormalizedInput::hash(meta.name, &input_bytes);
            let cache_hit = if cache.is_skipped(meta.name) {
                None
            } else {
                cache.lookup(&key).copied()
            };

            // Permission check fires first — denied tools never use cached results.
            let skills: &SkillRegistry = opts.skills.as_deref().unwrap_or(&EMPTY_SKILLS);
            let decision = check_with_skills(meta, &preview, prompter, skills).await;
            if decision.outcome == Outcome::Deny {
                // Drain any speculative slot to keep the registry clean.
                let _ = speculative.take(&id).await;
                tracing::warn!(
                    tool = %name,
                    reason = %decision.reason,
                    "tool denied"
                );
                return Err(LoopError::Denied(name.clone()));
            }

            if let Some(tx) = &opts.event_tx {
                let summary = tool_activity_summary(&name, &args);
                let diff_lines = tool_diff_lines(&name, &args);
                let _ = tx
                    .send(StreamEvent::ToolActivity {
                        tool: name.clone(),
                        summary,
                        diff_lines,
                    })
                    .await;
            }

            let result_bytes: Vec<u8> = if let Some(hit) = cache_hit {
                // Serve the cached body annotated with the originating turn.
                let store = opts.cas.as_ref().ok_or_else(|| {
                    LoopError::ToolFailure("memoization requires CAS to be configured".into())
                })?;
                let body = store
                    .get(origin_cas::Hash::from_bytes(hit.handle))
                    .map_err(|e| LoopError::ToolFailure(e.to_string()))?
                    .ok_or_else(|| LoopError::ToolFailure("cas miss on cached handle".into()))?;
                let annotated = format!(
                    "{}\n\n(cached from turn {})",
                    String::from_utf8_lossy(&body),
                    hit.from_turn,
                );
                // Drain any matching speculative slot so the task doesn't stay
                // detached — its result will be discarded in favour of the cache.
                let _ = speculative.take(&id).await;
                annotated.into_bytes()
            } else {
                // Try speculative precomputed result first; fall back to fresh
                // synchronous dispatch if the registry has no entry.
                if let Some(pre) = speculative.take(&id).await {
                    match pre {
                        Ok(bytes) => bytes,
                        Err(LoopError::BadArgs(msg) | LoopError::ToolFailure(msg)) => {
                            tracing::warn!(tool = %name, %msg, "speculative tool dispatch failed; returning error to model");
                            tool_results.push(Block::ToolResult {
                                tool_use_id: id,
                                handle: None,
                                inline: Some(format!("Error: {msg}").into_bytes()),
                                cache_marker: None,
                            });
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                } else if meta.name == "Bash" {
                    // Streaming dispatch path: forwards each stdout/stderr
                    // line to the CLI as a `ToolChunk` event as soon as
                    // the child writes it, so long-running commands no
                    // longer feel hung. The LLM still receives the fully
                    // accumulated body via `Block::ToolResult` below.
                    match run_bash_streaming(&args, opts.event_tx.as_ref()).await {
                        Ok(bytes) => bytes,
                        Err(msg) => {
                            tracing::warn!(tool = %name, %msg, "Bash dispatch failed; returning error to model");
                            if let Some(tx) = &opts.event_tx {
                                let _ = tx
                                    .send(StreamEvent::ToolResult {
                                        tool: name.clone(),
                                        ok: false,
                                        preview: msg.clone(),
                                        elided_bytes: 0,
                                    })
                                    .await;
                            }
                            tool_results.push(Block::ToolResult {
                                tool_use_id: id,
                                handle: None,
                                inline: Some(format!("Error: {msg}").into_bytes()),
                                cache_marker: None,
                            });
                            continue;
                        }
                    }
                } else {
                    match dispatch_tool(
                        meta,
                        &args,
                        opts.cas.as_deref(),
                        opts.code_graph.as_ref(),
                        opts.mem_router.as_ref().map(Arc::as_ref),
                        opts.memory_handle.as_deref(),
                        opts.coordinator.as_deref(),
                    )
                    .await
                    {
                        Ok(s) => s.into_bytes(),
                        Err(LoopError::BadArgs(msg) | LoopError::ToolFailure(msg)) => {
                            tracing::warn!(tool = %name, %msg, "tool dispatch failed; returning error to model");
                            // Surface the error to the CLI so the user sees
                            // *why* the tool stopped rather than a silent gap.
                            if let Some(tx) = &opts.event_tx {
                                let _ = tx
                                    .send(StreamEvent::ToolResult {
                                        tool: name.clone(),
                                        ok: false,
                                        preview: msg.clone(),
                                        elided_bytes: 0,
                                    })
                                    .await;
                            }
                            tool_results.push(Block::ToolResult {
                                tool_use_id: id,
                                handle: None,
                                inline: Some(format!("Error: {msg}").into_bytes()),
                                cache_marker: None,
                            });
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                }
            };

            // Stream a truncated preview of the result back to the CLI so the
            // user sees the tool's actual output. The LLM still consumes the
            // full body via the `Block::ToolResult` round-trip below.
            // Bash is excluded here — it manages its own completion event
            // inside `run_bash_streaming`, which emits a short `ToolResult`
            // only when zero chunks were streamed (silent commands like
            // write-only `powershell -File …` scripts), avoiding a redundant
            // trailing echo for verbose commands.
            if meta.name != "Bash" {
                if let Some(tx) = &opts.event_tx {
                    let (preview, elided) = build_tool_result_preview(&result_bytes);
                    let _ = tx
                        .send(StreamEvent::ToolResult {
                            tool: name.clone(),
                            ok: true,
                            preview,
                            elided_bytes: elided,
                        })
                        .await;
                }
            }

            let block = if let Some(cas) = opts.cas.as_ref() {
                let h: Hash = cas
                    .put(&result_bytes)
                    .map_err(|e| LoopError::ToolFailure(e.to_string()))?;

                // Phase 11 N4.3 wiring: register the freshly produced handle
                // with the active Plan so the Anthropic wire-encoder can
                // downgrade `Inline` → `Reference` for handles whose bodies
                // are stable enough to cache client-side. Heuristic: a Pure
                // tool's output is reusable across turns (`Sticky`);
                // a Mutating tool's output is a snapshot of state that just
                // changed (`Volatile`, the safe floor). Tools missing meta
                // (e.g. MCP-discovered runtime tools) inherit the floor.
                if let Some(plan) = opts.plan.as_ref() {
                    use origin_planner::Band;
                    use origin_tools::SideEffects;
                    let band = match meta.side_effects {
                        SideEffects::Pure => Band::Sticky,
                        SideEffects::Mutating => Band::Volatile,
                    };
                    plan.register_handle(*h.as_bytes(), band);
                }

                // Fire Extract job for large tool outputs (P5.3, N2.5.c).
                if result_bytes.len() >= origin_sidecar::extract::EXTRACT_THRESHOLD_BYTES {
                    if let Some(sidecar) = &opts.sidecar {
                        let _ = sidecar.submit(origin_sidecar::SidecarJob::Extract {
                            handle: h,
                            deliver_to: Box::new(NoopExtractDeliverer),
                        });
                    }
                }

                // Record into the memoization cache for subsequent turns
                // within this session. Skip-listed tools and hits are not
                // re-recorded (a hit means the entry is already present).
                if !cache.is_skipped(meta.name) && cache_hit.is_none() {
                    cache.record(key, *h.as_bytes(), turn);
                }

                Block::ToolResult {
                    tool_use_id: id,
                    handle: Some(*h.as_bytes()),
                    inline: None,
                    cache_marker: None,
                }
            } else {
                Block::ToolResult {
                    tool_use_id: id,
                    handle: None,
                    inline: Some(result_bytes),
                    cache_marker: None,
                }
            };
            tool_results.push(block);
        }

        // Append tool results as a single Role::Tool message (provider crates
        // will translate this to the right wire shape per provider).
        let mut tool_msg = Message::new(Role::Tool);
        tool_msg.blocks = tool_results;
        session.push(tool_msg);

        // Place a prompt-cache breakpoint at the freshly closed turn boundary
        // so the next iteration's `ChatRequest` (which re-sends the full
        // `session.snapshot()`) is billed against Anthropic's prompt cache
        // instead of as fresh input tokens. See [`apply_turn_cache_markers`].
        apply_turn_cache_markers(&mut session.messages, opts.plan.as_ref());
    }
    Err(LoopError::MaxTurns(opts.max_turns))
}

/// Anthropic's Messages API accepts at most 4 `cache_control` markers per
/// request. We stay strictly under that ceiling.
const MAX_CACHE_MARKERS: usize = 4;

/// Apply prompt-cache breakpoints after a completed agentic turn.
///
/// The Anthropic wire encoder has three independent emission paths for
/// `cache_control`: (1) planner-planted markers at `msg_idx == 0`, (2) the
/// per-block `cache_marker` field, and (3) the shared `Plan`'s
/// `dynamic_message_markers`. Each block emits at most one `cache_control`
/// via OR-combination, but the *count* of marker'd blocks is the union of
/// the positions selected by paths (2) and (3). If those paths select
/// different blocks the union can exceed Anthropic's per-request ceiling of
/// 4 markers, and the API rejects the request with `invalid_request_error:
/// "A maximum of 4 blocks with cache_control may be provided."`. That is the
/// production bug this helper guards against.
///
/// The fix is a single source of truth: pick the marker positions once, then
/// drive both the block-level field (path 2) and the plan's
/// `dynamic_message_markers` (path 3) from the same set. The selection
/// policy is "latest N turn boundaries", capped at [`MAX_CACHE_MARKERS`]:
/// latest-N is cache-optimal because Anthropic's prompt cache hits work on
/// prefix-extension — newer marker positions amortize across more subsequent
/// turns than older ones.
///
/// Without these markers every iteration of [`run_loop`] re-bills the full
/// `session.snapshot()` at the un-cached rate. Anthropic's prompt cache
/// charges 0.1× for cache reads, so engaging caching at stable turn
/// boundaries collapses the dominant cost of long agentic sessions.
fn apply_turn_cache_markers(messages: &mut [Message], plan: Option<&origin_planner::Plan>) {
    use origin_core::types::CacheBoundary;

    // Turn boundaries are the last `ToolResult` blocks of `Role::Tool`
    // messages. Each marks the close of one assistant turn's tool dispatch
    // round and is the natural place to cut a cache prefix.
    let turn_boundaries: Vec<(usize, usize)> = messages
        .iter()
        .enumerate()
        .filter_map(|(mi, msg)| {
            if !matches!(msg.role, Role::Tool) {
                return None;
            }
            msg.blocks
                .iter()
                .rposition(|b| matches!(b, Block::ToolResult { .. }))
                .map(|bi| (mi, bi))
        })
        .collect();

    // Single source of truth: the latest `MAX_CACHE_MARKERS` turn boundaries.
    // Empty session ⇒ no markers and the dynamic list is cleared so a previous
    // turn's stale state never leaks into the wire.
    let start = turn_boundaries.len().saturating_sub(MAX_CACHE_MARKERS);
    let chosen: Vec<(usize, usize)> = turn_boundaries[start..].to_vec();

    // Clear every existing block-level cache_marker across the session before
    // re-applying. This is the rotation: old marker positions that fall
    // outside the latest-N window are unmarked here. Without this pass, stale
    // markers from earlier calls would accumulate past the ceiling.
    for msg in messages.iter_mut() {
        for b in msg.blocks.iter_mut() {
            clear_block_cache_marker(b);
        }
    }

    // Path (2): block-level cache_marker on each chosen ToolResult block.
    for &(mi, bi) in &chosen {
        if let Block::ToolResult { cache_marker, .. } = &mut messages[mi].blocks[bi] {
            *cache_marker = Some(CacheBoundary::Sticky);
        }
    }

    // Path (3): mirror the same positions in the shared Plan's
    // `dynamic_message_markers`. The wire encoder OR-combines paths (2) and
    // (3) per block; because they target the same blocks here, the union
    // equals the intersection and the marker count is exactly `chosen.len()`.
    if let Some(plan) = plan {
        let msg_indices: Vec<usize> = chosen.iter().map(|&(mi, _)| mi).collect();
        plan.set_dynamic_message_markers(msg_indices);
    }
}

#[cfg(test)]
fn block_cache_marker_set(b: &Block) -> bool {
    match b {
        Block::Text { cache_marker, .. }
        | Block::ToolUse { cache_marker, .. }
        | Block::ToolResult { cache_marker, .. } => cache_marker.is_some(),
        Block::Thinking { .. } => false,
    }
}

fn clear_block_cache_marker(b: &mut Block) {
    match b {
        Block::Text { cache_marker, .. }
        | Block::ToolUse { cache_marker, .. }
        | Block::ToolResult { cache_marker, .. } => *cache_marker = None,
        Block::Thinking { .. } => {}
    }
}

#[cfg(test)]
mod cache_marker_tests {
    use super::*;
    use origin_core::types::{Block, Message, Role};
    use origin_planner::Plan;
    use std::collections::HashSet;

    /// Append one full user/assistant/tool turn to `msgs`.
    fn push_turn(msgs: &mut Vec<Message>, turn_idx: usize) {
        msgs.push(Message {
            role: Role::User,
            blocks: vec![Block::text(format!("user {turn_idx}"))],
        });
        msgs.push(Message {
            role: Role::Assistant,
            blocks: vec![Block::ToolUse {
                id: format!("u{turn_idx}"),
                name: "Read".into(),
                input_json: b"{}".to_vec(),
                cache_marker: None,
            }],
        });
        msgs.push(Message {
            role: Role::Tool,
            blocks: vec![Block::ToolResult {
                tool_use_id: format!("u{turn_idx}"),
                handle: None,
                inline: Some(b"ok".to_vec()),
                cache_marker: None,
            }],
        });
    }

    fn block_marked_message_indices(msgs: &[Message]) -> HashSet<usize> {
        msgs.iter()
            .enumerate()
            .filter_map(|(mi, m)| {
                if m.blocks.iter().any(block_cache_marker_set) {
                    Some(mi)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Anthropic rejects requests with > 4 `cache_control` markers. The wire
    /// encoder emits one marker per block when *any* of three independent paths
    /// fires for that block (block-level `cache_marker`, plan `marker_indices`,
    /// or plan `dynamic_message_markers`). If those paths target different
    /// blocks, their union can exceed 4 even when each path is individually
    /// capped — which is exactly the 5-marker 400 the daemon hit in production.
    ///
    /// Invariant: after `apply_turn_cache_markers`, the set of message indices
    /// marked at the block level must equal the set in `dynamic_message_markers`,
    /// and the cardinality must stay at or below `MAX_CACHE_MARKERS`.
    #[test]
    fn block_and_dynamic_markers_converge_under_ceiling_after_20_turns() {
        let plan = Plan::default();
        let mut msgs: Vec<Message> = Vec::new();
        for turn in 0..20 {
            push_turn(&mut msgs, turn);
            apply_turn_cache_markers(&mut msgs, Some(&plan));
        }

        let block_marked = block_marked_message_indices(&msgs);
        let dyn_marked: HashSet<usize> = plan.dynamic_message_markers().into_iter().collect();

        assert_eq!(
            block_marked, dyn_marked,
            "block-level markers and dynamic_message_markers must target the \
             same messages so the wire encoder's paths converge per block; got \
             block={block_marked:?}, dyn={dyn_marked:?}"
        );
        assert!(
            block_marked.len() <= MAX_CACHE_MARKERS,
            "marker count must stay at or below {MAX_CACHE_MARKERS} \
             (Anthropic's per-request ceiling); got {} at {block_marked:?}",
            block_marked.len()
        );
    }

    /// Same invariant at the smaller scale where bands collapse — exercises the
    /// edge cases of the recency classifier.
    #[test]
    fn block_and_dynamic_markers_converge_for_small_sessions() {
        for n_turns in 1..=6 {
            let plan = Plan::default();
            let mut msgs: Vec<Message> = Vec::new();
            for turn in 0..n_turns {
                push_turn(&mut msgs, turn);
                apply_turn_cache_markers(&mut msgs, Some(&plan));
            }
            let block_marked = block_marked_message_indices(&msgs);
            let dyn_marked: HashSet<usize> = plan.dynamic_message_markers().into_iter().collect();
            assert_eq!(
                block_marked, dyn_marked,
                "divergence at n_turns={n_turns}: block={block_marked:?}, dyn={dyn_marked:?}"
            );
            assert!(
                block_marked.len() <= MAX_CACHE_MARKERS,
                "over ceiling at n_turns={n_turns}: {block_marked:?}"
            );
        }
    }
}

/// Rebuild entry-point invoked by the future IPC handler / git hook.
///
/// P7.8 ships the free function; P10 wires it into the daemon's `Frame`
/// dispatcher alongside [`crate::protocol::RebuildRequest`]. The function
/// itself is a thin shim over [`origin_codegraph::rebuild::rebuild_paths`].
///
/// # Errors
/// Propagates [`origin_codegraph::rebuild::RebuildError`] for fatal CAS /
/// `SQLite` failures; per-file errors are aggregated into the returned report.
// `req` is taken by value to match the future IPC handler shape — once P10
// deserializes a `RebuildRequest` off the wire it will move the value into
// this function. Taking by reference now would force a copy at the boundary.
#[allow(clippy::needless_pass_by_value)]
pub fn rebuild_codegraph(
    idx: &mut origin_codegraph::index::CodeGraphIndex,
    req: crate::protocol::RebuildRequest,
    lang: origin_codegraph::Language,
) -> Result<origin_codegraph::rebuild::RebuildReport, origin_codegraph::rebuild::RebuildError> {
    tracing::info!(paths = req.paths.len(), "rebuild_codegraph: dispatching");
    origin_codegraph::rebuild::rebuild_paths(idx, &req.paths, lang)
}

#[tracing::instrument(
    level = "info",
    skip(args, cas, code_graph, mem_router, memory, coordinator),
    fields(kind = "tool", tool = meta.name)
)]
// dispatch arm-per-tool registry; splitting would obscure tool->arm mapping.
#[allow(clippy::too_many_lines)]
async fn dispatch_tool(
    meta: &ToolMeta,
    args: &Value,
    cas: Option<&Store>,
    code_graph: Option<&Arc<tokio::sync::Mutex<origin_codegraph::index::CodeGraphIndex>>>,
    mem_router: Option<&dyn origin_codegraph::ask::MemRouter>,
    memory: Option<&dyn MemoryHandle>,
    coordinator: Option<&origin_swarm::Coordinator>,
) -> Result<String, LoopError> {
    match meta.name {
        "Read" => {
            let args = origin_tools::builtins::read::ReadArgs {
                file_path: args
                    .get("file_path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Read: missing `file_path`".into()))?
                    .to_string(),
                offset: args.get("offset").and_then(Value::as_u64).map(|n| n as u32),
                limit: args.get("limit").and_then(Value::as_u64).map(|n| n as u32),
                as_: args.get("as").and_then(Value::as_str).map(str::to_string),
            };
            origin_tools::builtins::read::read_v2(args).map_err(|e| LoopError::ToolFailure(e.message))
        }
        "Glob" => {
            let gargs = origin_tools::builtins::glob_tool::GlobArgs {
                pattern: args
                    .get("pattern")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Glob: missing `pattern`".into()))?
                    .to_string(),
                path: args.get("path").and_then(Value::as_str).map(str::to_string),
                head_limit: args.get("head_limit").and_then(Value::as_u64).map(|n| n as u32),
            };
            origin_tools::builtins::glob_tool::glob_v2(gargs)
                .map(|v| serde_json::to_string(&v).unwrap())
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        "Grep" => {
            let mode = args.get("output_mode").and_then(Value::as_str).map(|s| match s {
                "files_with_matches" => origin_tools::builtins::grep_tool::OutputMode::FilesWithMatches,
                "content" => origin_tools::builtins::grep_tool::OutputMode::Content,
                "count" => origin_tools::builtins::grep_tool::OutputMode::Count,
                _ => origin_tools::builtins::grep_tool::OutputMode::FilesWithMatches,
            });
            let gargs = origin_tools::builtins::grep_tool::GrepArgs {
                pattern: args
                    .get("pattern")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Grep: missing `pattern`".into()))?
                    .to_string(),
                path: args.get("path").and_then(Value::as_str).map(str::to_string),
                glob: args.get("glob").and_then(Value::as_str).map(str::to_string),
                r#type: args.get("type").and_then(Value::as_str).map(str::to_string),
                output_mode: mode,
                head_limit: args.get("head_limit").and_then(Value::as_u64).map(|n| n as u32),
                before: args
                    .get("before")
                    .and_then(Value::as_u64)
                    .map(|n| n as u32)
                    .unwrap_or(0),
                after: args
                    .get("after")
                    .and_then(Value::as_u64)
                    .map(|n| n as u32)
                    .unwrap_or(0),
                line_numbers: args.get("line_numbers").and_then(Value::as_bool).unwrap_or(false),
                multiline: args.get("multiline").and_then(Value::as_bool).unwrap_or(false),
            };
            origin_tools::builtins::grep_tool::grep_v2(gargs)
                .map(|v| serde_json::to_string(&v).unwrap())
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        "Edit" => {
            let args = origin_tools::builtins::edit::EditArgs {
                file_path: args
                    .get("file_path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Edit: missing `file_path`".into()))?
                    .to_string(),
                old_string: args
                    .get("old_string")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Edit: missing `old_string`".into()))?
                    .to_string(),
                new_string: args
                    .get("new_string")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Edit: missing `new_string`".into()))?
                    .to_string(),
                replace_all: args.get("replace_all").and_then(Value::as_bool).unwrap_or(false),
            };
            origin_tools::builtins::edit::edit_v2(args)
                .map(|v| serde_json::to_string(&v).unwrap())
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        "MultiEdit" => {
            let edits_v = args
                .get("edits")
                .and_then(Value::as_array)
                .ok_or_else(|| LoopError::BadArgs("MultiEdit: missing `edits`".into()))?;
            let edits = edits_v
                .iter()
                .map(|e| {
                    let o = e.get("old").and_then(Value::as_str).unwrap_or("");
                    let n = e.get("new").and_then(Value::as_str).unwrap_or("");
                    let r = e.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
                    origin_tools::builtins::multi_edit::EditOp {
                        old: o.into(),
                        new: n.into(),
                        replace_all: r,
                    }
                })
                .collect();
            let margs = origin_tools::builtins::multi_edit::MultiEditArgs {
                file_path: args
                    .get("file_path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("MultiEdit: missing `file_path`".into()))?
                    .to_string(),
                edits,
            };
            origin_tools::builtins::multi_edit::multi_edit(&margs)
                .map(|v| serde_json::to_string(&v).unwrap())
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        "ApplyPatch" => {
            let pargs = origin_tools::builtins::apply_patch::ApplyPatchArgs {
                patch: args
                    .get("patch")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("ApplyPatch: missing `patch`".into()))?
                    .to_string(),
            };
            origin_tools::builtins::apply_patch::apply_patch(&pargs)
                .map(|v| serde_json::to_string(&v).unwrap())
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        "Write" => {
            let guard = origin_tools::builtins::write::WriteGuard::default();
            // Production callers pass the session's guard via dispatch_with_envelope;
            // this passthrough path is used only by tests that bypass the envelope.
            let args = origin_tools::builtins::write::WriteArgs {
                file_path: args
                    .get("file_path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Write: missing `file_path`".into()))?
                    .to_string(),
                content: args
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Write: missing `content`".into()))?
                    .to_string(),
                force: args.get("force").and_then(Value::as_bool).unwrap_or(false),
            };
            origin_tools::builtins::write::write_v2(args, &guard)
                .map(|()| "write ok".to_string())
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        "Bash" => {
            let bargs = origin_tools::builtins::bash::BashArgs {
                command: args
                    .get("command")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Bash: missing `command`".into()))?
                    .to_string(),
                timeout: args.get("timeout").and_then(Value::as_u64).map(|n| n as u32),
                cwd: args.get("cwd").and_then(Value::as_str).map(str::to_string),
                env: args
                    .get("env")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|e| {
                                let arr = e.as_array()?;
                                Some((
                                    arr.first()?.as_str()?.to_string(),
                                    arr.get(1)?.as_str()?.to_string(),
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                run_in_background: args
                    .get("run_in_background")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            };
            // Local supervisor for the passthrough path; the envelope path
            // uses ctx.supervisor (shared across calls within a session).
            // NOTE: run_in_background + Monitor across separate tool
            // invocations won't work via this path — see known limitations.
            let sup = origin_tools::proc_supervisor::Supervisor::new();
            origin_tools::builtins::bash::bash_v2(bargs, &sup)
                .await
                .map(|v| serde_json::to_string(&v).unwrap())
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        "Monitor" => {
            let margs = origin_tools::builtins::monitor::MonitorArgs {
                pid: args
                    .get("pid")
                    .and_then(Value::as_u64)
                    .map(|n| n as u32)
                    .ok_or_else(|| LoopError::BadArgs("Monitor: missing `pid`".into()))?,
                since_byte: args.get("since_byte").and_then(Value::as_u64).unwrap_or(0),
                max_bytes: args
                    .get("max_bytes")
                    .and_then(Value::as_u64)
                    .map(|n| n as u32)
                    .unwrap_or(4096),
                wait: args.get("wait").and_then(Value::as_bool).unwrap_or(false),
            };
            // Envelope-routed path uses ctx.supervisor; this passthrough
            // makes a stub supervisor that always returns unknown_pid.
            // Production should reach this arm only when run_in_background
            // was used — until Phase 8 wires the shared supervisor.
            let sup = origin_tools::proc_supervisor::Supervisor::new();
            origin_tools::builtins::monitor::monitor(margs, &sup)
                .await
                .map(|v| serde_json::to_string(&v).unwrap())
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        "Diagnostics" => {
            // Envelope-routed path uses ctx.ra (shared handle); this passthrough
            // path constructs a per-call DaemonRa. Known limitation — Phase 8
            // will wire the shared handle via dispatch_with_envelope.
            // NOTE: per-call DaemonRa means RA is re-spawned every call.
            let sev = match args.get("severity").and_then(Value::as_str).unwrap_or("any") {
                "error" => origin_tools::ra_bridge::Severity::Error,
                "warning" => origin_tools::ra_bridge::Severity::Warning,
                "hint" => origin_tools::ra_bridge::Severity::Hint,
                _ => origin_tools::ra_bridge::Severity::Any,
            };
            let dargs = origin_tools::builtins::diagnostics::DiagnosticsArgs {
                path: args.get("path").and_then(Value::as_str).map(str::to_string),
                severity: sev,
            };
            let ra = crate::ra_impl::DaemonRa::new(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            );
            origin_tools::builtins::diagnostics::diagnostics(dargs, &ra)
                .await
                .map(|v| serde_json::to_string(&v).unwrap())
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        "ToolSearch" => {
            let sargs = origin_tools::builtins::tool_search::ToolSearchArgs {
                query: args
                    .get("query")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("ToolSearch: missing `query`".into()))?
                    .to_string(),
                max_results: args.get("max_results").and_then(Value::as_u64).map(|n| n as u32),
            };
            origin_tools::builtins::tool_search::tool_search(&sargs)
                .map(|v| serde_json::to_string(&v).unwrap())
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        "Recall" => {
            let store =
                cas.ok_or_else(|| LoopError::ToolFailure("Recall requires CAS to be configured".into()))?;
            let handle_hex = args
                .get("handle")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Recall: missing `handle`".into()))?;
            let handle: [u8; 32] = {
                let mut buf = [0u8; 32];
                hex::decode_to_slice(handle_hex, &mut buf)
                    .map_err(|e| LoopError::BadArgs(format!("Recall: bad hex: {e}")))?;
                buf
            };
            let region = args.get("region").map(parse_region).transpose()?;
            origin_tools::builtins::recall::recall_tool(store, handle, region)
                .map_err(|e| LoopError::ToolFailure(e.to_string()))
        }
        // ── Code-graph tools ──
        "graph_query" => {
            use origin_codegraph::index::EntityId;
            use origin_codegraph::query::Query;
            let idx_arc = code_graph.ok_or_else(|| {
                LoopError::ToolFailure(
                    "graph_query: code-graph subsystem not yet wired (CodeGraphIndex not in LoopOptions)."
                        .into(),
                )
            })?;
            let kind = args
                .get("kind")
                .and_then(Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("graph_query: missing `kind`".into()))?;
            let q_args = args.get("args").cloned().unwrap_or(Value::Null);
            let parse_id = |v: &Value, field: &str| -> Result<EntityId, LoopError> {
                let s = v
                    .as_str()
                    .ok_or_else(|| LoopError::BadArgs(format!("graph_query.{field}: not a string")))?;
                let mut buf = [0u8; 32];
                hex::decode_to_slice(s, &mut buf)
                    .map_err(|e| LoopError::BadArgs(format!("graph_query.{field}: bad hex: {e}")))?;
                Ok(EntityId(buf))
            };
            let q = match kind {
                "path" => Query::Path {
                    from: parse_id(&q_args["from"], "args.from")?,
                    to: parse_id(&q_args["to"], "args.to")?,
                    max_hops: usize::try_from(q_args["max_hops"].as_u64().unwrap_or(8)).unwrap_or(usize::MAX),
                },
                "neighbors" => Query::Neighbors {
                    node: parse_id(&q_args["node"], "args.node")?,
                    depth: usize::try_from(q_args["depth"].as_u64().unwrap_or(1)).unwrap_or(usize::MAX),
                },
                "communities" => Query::Communities,
                "god_nodes" => Query::GodNodes {
                    top_per_partition: usize::try_from(q_args["top_per_partition"].as_u64().unwrap_or(3))
                        .unwrap_or(usize::MAX),
                },
                "recent_changes" => Query::RecentChanges {
                    since_ms: q_args["since_ms"].as_i64().unwrap_or(0),
                },
                other => return Err(LoopError::BadArgs(format!("graph_query: unknown kind `{other}`"))),
            };
            let result = {
                let idx = idx_arc.lock().await;
                origin_tools::builtins::graph_query::graph_query_tool(&idx, q)
                    .map_err(|e| LoopError::ToolFailure(e.to_string()))?
            };
            Ok(serialize_query_result(&result))
        }
        "graph_path" => {
            use origin_codegraph::index::EntityId;
            let idx_arc = code_graph.ok_or_else(|| {
                LoopError::ToolFailure(
                    "graph_path: code-graph subsystem not yet wired (CodeGraphIndex not in LoopOptions)."
                        .into(),
                )
            })?;
            let parse_hex_id = |field: &str| -> Result<EntityId, LoopError> {
                let s = args
                    .get(field)
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs(format!("graph_path: missing `{field}`")))?;
                let mut buf = [0u8; 32];
                hex::decode_to_slice(s, &mut buf)
                    .map_err(|e| LoopError::BadArgs(format!("graph_path.{field}: bad hex: {e}")))?;
                Ok(EntityId(buf))
            };
            let from = parse_hex_id("from")?;
            let to = parse_hex_id("to")?;
            let max_hops = usize::try_from(args.get("max_hops").and_then(Value::as_u64).unwrap_or(8))
                .unwrap_or(usize::MAX);
            let result = {
                let idx = idx_arc.lock().await;
                origin_tools::builtins::graph_path::graph_path_tool(&idx, from, to, max_hops)
                    .map_err(|e| LoopError::ToolFailure(e.to_string()))?
            };
            Ok(serialize_query_result(&result))
        }
        "graph_summarize" => {
            let idx_arc = code_graph.ok_or_else(|| {
                LoopError::ToolFailure(
                    "graph_summarize: code-graph subsystem not yet wired (CodeGraphIndex not in LoopOptions)."
                        .into(),
                )
            })?;
            // `community_id` (int) or `node` (hex) is the target string.
            let target = args
                .get("community_id")
                .and_then(Value::as_i64)
                .map(|cid| cid.to_string())
                .or_else(|| args.get("node").and_then(Value::as_str).map(ToString::to_string))
                .unwrap_or_default();
            // graph_summarize_tool always returns QueryResult::Empty at P7.8.
            {
                let idx = idx_arc.lock().await;
                let _result = origin_tools::builtins::graph_summarize::graph_summarize_tool(&idx, target);
            }
            Ok("{}".to_string())
        }
        "graph_rebuild" => {
            use std::path::PathBuf;
            let idx_arc = code_graph.ok_or_else(|| {
                LoopError::ToolFailure(
                    "graph_rebuild: code-graph subsystem not yet wired (CodeGraphIndex not in LoopOptions)."
                        .into(),
                )
            })?;
            let paths: Vec<PathBuf> = args
                .get("paths")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(PathBuf::from)).collect())
                .unwrap_or_default();
            // Lock, mutate, then release before returning — don't hold across any await.
            let report = {
                let mut idx = idx_arc.lock().await;
                origin_tools::builtins::graph_rebuild::graph_rebuild_tool(
                    &mut idx,
                    paths,
                    origin_codegraph::Language::Rust,
                )
                .map_err(|e| LoopError::ToolFailure(e.to_string()))?
            };
            Ok(serde_json::json!({
                "paths_seen": report.paths_seen,
                "nodes_added": report.nodes_added,
                "nodes_updated": report.nodes_updated,
                "errors": report.errors,
            })
            .to_string())
        }
        // `graph_explain` has zero infrastructure dependency — it just classifies
        // a typed `Query` into a deterministic English gloss. Wired here as a
        // real call so the model gets a working tool, not a stub error.
        "graph_explain" => {
            use origin_codegraph::index::EntityId;
            use origin_codegraph::query::Query;
            let kind = args
                .get("kind")
                .and_then(Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("graph_explain: missing `kind`".into()))?;
            let q_args = args.get("args").cloned().unwrap_or(Value::Null);
            let parse_id = |v: &Value, field: &str| -> Result<EntityId, LoopError> {
                let s = v
                    .as_str()
                    .ok_or_else(|| LoopError::BadArgs(format!("graph_explain.{field}: not a string")))?;
                let mut buf = [0u8; 32];
                hex::decode_to_slice(s, &mut buf)
                    .map_err(|e| LoopError::BadArgs(format!("graph_explain.{field}: bad hex: {e}")))?;
                Ok(EntityId(buf))
            };
            let q = match kind {
                "path" => Query::Path {
                    from: parse_id(&q_args["from"], "args.from")?,
                    to: parse_id(&q_args["to"], "args.to")?,
                    max_hops: usize::try_from(q_args["max_hops"].as_u64().unwrap_or(8)).unwrap_or(usize::MAX),
                },
                "neighbors" => Query::Neighbors {
                    node: parse_id(&q_args["node"], "args.node")?,
                    depth: usize::try_from(q_args["depth"].as_u64().unwrap_or(1)).unwrap_or(usize::MAX),
                },
                "communities" => Query::Communities,
                "god_nodes" => Query::GodNodes {
                    top_per_partition: usize::try_from(q_args["top_per_partition"].as_u64().unwrap_or(3))
                        .unwrap_or(usize::MAX),
                },
                "recent_changes" => Query::RecentChanges {
                    since_ms: q_args["since_ms"].as_i64().unwrap_or(0),
                },
                other => {
                    return Err(LoopError::BadArgs(format!(
                        "graph_explain: unknown kind `{other}`"
                    )))
                }
            };
            Ok(origin_tools::builtins::graph_explain::graph_explain_tool(&q))
        }
        // ── Memory tools ──
        // `mem_search` / `mem_save` / `mem_forget` require a `&dyn MemoryHandle`
        // threaded through `LoopOptions::memory_handle`. When the handle is
        // `Some`, they delegate to the typed execute functions in
        // `origin_tools::builtins::mem`. When `None`, they return a clear
        // `ToolFailure` (never `UnknownTool`) so the model knows the subsystem
        // exists but is not currently configured.
        "mem_search" => {
            let Some(handle) = memory else {
                return Err(LoopError::ToolFailure(
                    "mem_search: memory subsystem not configured".into(),
                ));
            };
            let input = args.to_string();
            origin_tools::builtins::mem::mem_search_execute(handle, &input)
                .await
                .map_err(|e| LoopError::ToolFailure(e.to_string()))
        }
        "mem_save" => {
            let Some(handle) = memory else {
                return Err(LoopError::ToolFailure(
                    "mem_save: memory subsystem not configured".into(),
                ));
            };
            let input = args.to_string();
            origin_tools::builtins::mem::mem_save_execute(handle, &input)
                .await
                .map_err(|e| LoopError::ToolFailure(e.to_string()))
        }
        "mem_forget" => {
            let Some(handle) = memory else {
                return Err(LoopError::ToolFailure(
                    "mem_forget: memory subsystem not configured".into(),
                ));
            };
            let input = args.to_string();
            origin_tools::builtins::mem::mem_forget_execute(handle, &input)
                .await
                .map_err(|e| LoopError::ToolFailure(e.to_string()))
        }
        // ── ask ──
        "ask" => {
            let idx_arc = code_graph.ok_or_else(|| {
                LoopError::ToolFailure(
                    "ask: code-graph subsystem not yet wired (CodeGraphIndex not in LoopOptions).".into(),
                )
            })?;
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("ask: missing `query`".into()))?;
            // Use provided MemRouter or fall back to the NullMemRouter.
            let null_router = origin_codegraph::ask::NullMemRouter;
            let router: &dyn origin_codegraph::ask::MemRouter = mem_router.unwrap_or(&null_router);
            let result = {
                let idx = idx_arc.lock().await;
                origin_tools::builtins::ask::ask_tool(&idx, router, query)
            };
            let route_str = match result.route {
                origin_codegraph::ask::Route::Code => "code",
                origin_codegraph::ask::Route::Mem => "mem",
                origin_codegraph::ask::Route::Both => "both",
            };
            let mem_hits: Vec<serde_json::Value> = result
                .mem_hits
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "id": h.id,
                        "score": h.score,
                        "body": h.body,
                    })
                })
                .collect();
            Ok(serde_json::json!({
                "route": route_str,
                "mem_hits": mem_hits,
            })
            .to_string())
        }
        // ── Web tools (stateless) ──
        "WebFetch" => {
            let url = args
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("WebFetch: missing `url`".into()))?;
            origin_tools::builtins::web_fetch::web_fetch(url)
                .await
                .map_err(LoopError::ToolFailure)
        }
        "WebSearch" => {
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("WebSearch: missing `query`".into()))?;
            let count =
                usize::try_from(args.get("count").and_then(Value::as_u64).unwrap_or(10)).unwrap_or(10);
            let hits = origin_tools::builtins::web_search::web_search(query, count)
                .await
                .map_err(LoopError::ToolFailure)?;
            serde_json::to_string(&hits).map_err(|e| LoopError::ToolFailure(format!("WebSearch: json: {e}")))
        }
        // ── Browser (stateful; lazy process-global router) ──
        //
        // The router owns two long-lived Node child processes (agent-browser
        // primary, CloakBrowser fallback). Spawning them is expensive, so we
        // initialize the router once per process via `OnceCell`. Concurrent
        // browser calls serialize on the Mutex — fine because the agent loop
        // is sequential within a turn, and sessions are disambiguated by the
        // `session` field in each verb.
        "Browser" => {
            use tokio::sync::{Mutex, OnceCell};
            static ROUTER: OnceCell<Mutex<origin_browser::BrowserRouter>> = OnceCell::const_new();
            let router_mu = ROUTER
                .get_or_try_init(|| async {
                    origin_browser::BrowserRouter::new()
                        .await
                        .map(Mutex::new)
                        .map_err(|e| LoopError::ToolFailure(format!("Browser router init: {e}")))
                })
                .await?;
            let verb: origin_browser::Verb = serde_json::from_value(args.clone())
                .map_err(|e| LoopError::BadArgs(format!("Browser: {e}")))?;
            let resp = {
                let mut r = router_mu.lock().await;
                r.run(&verb)
                    .await
                    .map_err(|e| LoopError::ToolFailure(format!("Browser: {e}")))?
            };
            serde_json::to_string(&resp).map_err(|e| LoopError::ToolFailure(format!("Browser: json: {e}")))
        }
        // ── Task ──
        // Requires an `origin_swarm::Coordinator` threaded through `LoopOptions`.
        "Task" => {
            let coord =
                coordinator.ok_or_else(|| LoopError::ToolFailure("Task subsystem not configured".into()))?;
            let input: origin_tools::builtins::task::TaskInput =
                serde_json::from_value(args.clone()).map_err(|e| LoopError::BadArgs(format!("Task: {e}")))?;
            let output = origin_tools::builtins::task::task_tool(coord, input)
                .await
                .map_err(|e| LoopError::ToolFailure(e.to_string()))?;
            serde_json::to_string(&output).map_err(|e| LoopError::ToolFailure(format!("Task: json: {e}")))
        }
        other => Err(LoopError::UnknownTool(other.into())),
    }
}

/// Serialize a [`origin_codegraph::query::QueryResult`] as a JSON string.
///
/// `NodeRow` handles (`signature_handle`, `body_handle`) are rendered as
/// lowercase hex so they are round-trippable through the CAS layer.
fn serialize_query_result(r: &origin_codegraph::query::QueryResult) -> String {
    use origin_codegraph::query::QueryResult;
    match r {
        QueryResult::Empty => "{}".to_string(),
        QueryResult::Nodes(nodes) => {
            let arr: Vec<serde_json::Value> = nodes.iter().map(node_row_to_json).collect();
            serde_json::json!({ "nodes": arr }).to_string()
        }
        QueryResult::Path(nodes) => {
            let arr: Vec<serde_json::Value> = nodes.iter().map(node_row_to_json).collect();
            serde_json::json!({ "path": arr }).to_string()
        }
        QueryResult::Partitions(parts) => {
            let arr: Vec<serde_json::Value> = parts
                .iter()
                .map(|part| {
                    let rows: Vec<serde_json::Value> = part.iter().map(node_row_to_json).collect();
                    serde_json::Value::Array(rows)
                })
                .collect();
            serde_json::json!({ "partitions": arr }).to_string()
        }
    }
}

fn node_row_to_json(row: &origin_codegraph::index::NodeRow) -> serde_json::Value {
    serde_json::json!({
        "entity_id": hex::encode(row.entity_id.as_bytes()),
        "kind": row.kind,
        "name": row.name,
        "file_path": row.file_path,
        "signature_handle": hex::encode(row.signature_handle),
        "body_handle": hex::encode(row.body_handle),
    })
}

/// Dispatch the `Bash` tool via `bash_v2` and forward buffered output lines
/// to `event_tx` as [`StreamEvent::ToolChunk`] events. Returns the
/// serialised JSON result bytes so the LLM sees the full structured output.
/// `event_tx` being `None` (unit tests, headless runs) is supported — chunks
/// are skipped but execution still completes.
async fn run_bash_streaming(
    args: &Value,
    event_tx: Option<&tokio::sync::mpsc::Sender<StreamEvent>>,
) -> Result<Vec<u8>, String> {
    let bargs = origin_tools::builtins::bash::BashArgs {
        command: args
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| "Bash: missing `command`".to_string())?
            .to_string(),
        timeout: args.get("timeout").and_then(Value::as_u64).map(|n| n as u32),
        cwd: args.get("cwd").and_then(Value::as_str).map(str::to_string),
        env: args
            .get("env")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|e| {
                        let arr = e.as_array()?;
                        Some((
                            arr.first()?.as_str()?.to_string(),
                            arr.get(1)?.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        run_in_background: args
            .get("run_in_background")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    // Per-call supervisor: run_in_background + Monitor across separate tool
    // invocations won't work via this legacy path (known limitation —
    // Phase 8 replaces with envelope-level shared supervisor).
    let sup = origin_tools::proc_supervisor::Supervisor::new();
    let result = origin_tools::builtins::bash::bash_v2(bargs, &sup)
        .await
        .map_err(|e| e.message)?;

    // Forward stdout lines as ToolChunk events.
    let stdout_str = result["stdout"].as_str().unwrap_or("");
    let exit_code = result["exit_code"].as_i64().unwrap_or(-1);
    let mut chunk_count: u32 = 0;
    if let Some(tx) = event_tx {
        for line in stdout_str.lines() {
            chunk_count += 1;
            let _ = tx
                .send(StreamEvent::ToolChunk {
                    tool: "Bash".to_string(),
                    content: line.to_string(),
                })
                .await;
        }
        if chunk_count == 0 {
            let _ = tx
                .send(StreamEvent::ToolResult {
                    tool: "Bash".to_string(),
                    ok: exit_code == 0,
                    preview: format!("(exit {exit_code}, no output)"),
                    elided_bytes: 0,
                })
                .await;
        }
    }

    Ok(serde_json::to_string(&result).unwrap().into_bytes())
}

/// Build a bounded preview of a tool's result bytes for live display in the
/// CLI. Returns `(preview, elided_bytes)`. The preview is at most
/// `MAX_PREVIEW_LINES` lines, each truncated to `MAX_PREVIEW_LINE_CHARS`
/// chars; the elided byte count covers everything past that window so the
/// CLI can render a "+N bytes omitted" affordance. Non-UTF8 input is
/// lossily decoded — the model still sees the raw bytes upstream.
fn build_tool_result_preview(bytes: &[u8]) -> (String, u32) {
    const MAX_PREVIEW_LINES: usize = 8;
    const MAX_PREVIEW_LINE_CHARS: usize = 200;

    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut consumed: usize = 0;
    let mut lines_iter = text.split_inclusive('\n');
    for _ in 0..MAX_PREVIEW_LINES {
        let Some(line) = lines_iter.next() else {
            break;
        };
        consumed += line.len();
        let trimmed: String = line.chars().take(MAX_PREVIEW_LINE_CHARS).collect();
        out.push_str(&trimmed);
        if trimmed.len() < line.len() && !trimmed.ends_with('\n') {
            out.push('\n');
        }
    }
    // Remove a single trailing newline so the CLI doesn't render a stray
    // blank line — the renderer adds its own line breaks per scrollback row.
    if out.ends_with('\n') {
        out.pop();
    }
    let elided = bytes.len().saturating_sub(consumed);
    (out, u32::try_from(elided).unwrap_or(u32::MAX))
}

fn tool_activity_summary(name: &str, args: &Value) -> String {
    let path_str = || {
        args.get("path")
            .or_else(|| args.get("file_path"))
            .and_then(Value::as_str)
            .unwrap_or("")
    };
    match name {
        "Write" => {
            let path = path_str();
            let lines = args
                .get("content")
                .and_then(Value::as_str)
                .map_or(0, |c| c.lines().count());
            format!("{path} ({lines} lines)")
        }
        "Edit" => {
            let path = path_str();
            let old_lines = args
                .get("old_string")
                .and_then(Value::as_str)
                .map_or(0, |s| s.lines().count());
            let new_lines = args
                .get("new_string")
                .and_then(Value::as_str)
                .map_or(0, |s| s.lines().count());
            let added = new_lines.saturating_sub(old_lines);
            let removed = old_lines.saturating_sub(new_lines);
            let mut parts = Vec::new();
            if added > 0 {
                parts.push(format!("+{added}"));
            }
            if removed > 0 {
                parts.push(format!("-{removed}"));
            }
            if parts.is_empty() {
                parts.push(format!("~{new_lines}"));
            }
            format!("{path} ({} lines)", parts.join(", "))
        }
        "Read" => path_str().to_string(),
        "Grep" => {
            let pat = args.get("pattern").and_then(Value::as_str).unwrap_or("");
            let root = args.get("root").and_then(Value::as_str).unwrap_or(".");
            // Cap the pattern so a long regex doesn't blow past the
            // status column. Root is short by nature.
            let pat_short: String = pat.chars().take(40).collect();
            format!("{pat_short} @ {root}")
        }
        "Glob" => args
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        "Bash" => {
            let cmd = args.get("command").and_then(Value::as_str).unwrap_or("");
            cmd.chars().take(80).collect()
        }
        "WebFetch" => args.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
        _ => {
            let s = args.to_string();
            s.chars().take(60).collect()
        }
    }
}

fn tool_diff_lines(name: &str, args: &Value) -> Vec<crate::protocol::DiffLine> {
    use crate::protocol::DiffLine;
    match name {
        "Edit" => {
            let old = args.get("old_string").and_then(Value::as_str).unwrap_or("");
            let new = args.get("new_string").and_then(Value::as_str).unwrap_or("");
            let old_lines: Vec<&str> = old.lines().collect();
            let new_lines: Vec<&str> = new.lines().collect();
            let mut out = Vec::new();
            let mut old_i = 0u32;
            let mut new_i = 0u32;
            let max_old = old_lines.len();
            let max_new = new_lines.len();
            let mut oi = 0;
            let mut ni = 0;
            while oi < max_old || ni < max_new {
                if oi < max_old && ni < max_new && old_lines[oi] == new_lines[ni] {
                    old_i += 1;
                    new_i += 1;
                    out.push(DiffLine {
                        kind: " ".to_string(),
                        line_no: new_i,
                        text: new_lines[ni].to_string(),
                    });
                    oi += 1;
                    ni += 1;
                } else {
                    while oi < max_old && (ni >= max_new || !new_lines[ni..].contains(&old_lines[oi])) {
                        old_i += 1;
                        out.push(DiffLine {
                            kind: "-".to_string(),
                            line_no: old_i,
                            text: old_lines[oi].to_string(),
                        });
                        oi += 1;
                    }
                    while ni < max_new && (oi >= max_old || !old_lines[oi..].contains(&new_lines[ni])) {
                        new_i += 1;
                        out.push(DiffLine {
                            kind: "+".to_string(),
                            line_no: new_i,
                            text: new_lines[ni].to_string(),
                        });
                        ni += 1;
                    }
                }
            }
            out
        }
        "Write" => {
            let content = args.get("content").and_then(Value::as_str).unwrap_or("");
            content
                .lines()
                .enumerate()
                .map(|(i, line)| DiffLine {
                    kind: "+".to_string(),
                    line_no: (i + 1) as u32,
                    text: line.to_string(),
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

fn parse_region(v: &Value) -> Result<origin_tools::builtins::recall::Region, LoopError> {
    if let Some(lines) = v.get("lines").and_then(Value::as_array) {
        // Region indices are bounded by file sizes and will never exceed usize::MAX
        // on any supported target. Casting u64 -> usize is intentional here.
        #[allow(clippy::cast_possible_truncation)]
        let start = lines
            .first()
            .and_then(Value::as_u64)
            .ok_or_else(|| LoopError::BadArgs("Recall.region.lines requires [start, end]".into()))?
            as usize;
        #[allow(clippy::cast_possible_truncation)]
        let end = lines
            .get(1)
            .and_then(Value::as_u64)
            .ok_or_else(|| LoopError::BadArgs("Recall.region.lines requires [start, end]".into()))?
            as usize;
        Ok(origin_tools::builtins::recall::Region::Lines { start, end })
    } else if let Some(m) = v.get("match").and_then(Value::as_str) {
        Ok(origin_tools::builtins::recall::Region::Match {
            pattern: m.to_string(),
        })
    } else if v.get("outline_only").and_then(Value::as_bool) == Some(true) {
        Ok(origin_tools::builtins::recall::Region::OutlineOnly)
    } else {
        Err(LoopError::BadArgs(
            "Recall.region: expected lines/match/outline_only".into(),
        ))
    }
}

/// Run one streaming turn. Pre-subscribes BOTH the drain and (optionally) the
/// relay before publishing — a fresh subscriber starts at the current write
/// cursor, so subscribing after the producer publishes would miss every record.
/// The drive future always closes the ring on completion (success OR error) so
/// the drain and the relay subscriber wake up cleanly even if the provider
/// fails mid-stream. `Ring::close` is idempotent.
///
/// P3.4: also drives `ToolUseParser`s and spawns speculative tasks for pure
/// tools when the first `Field` event fires. Returns the registry alongside
/// the synthetic `ChatResponse` so `run_loop` can await precomputed handles.
#[tracing::instrument(
    level = "info",
    skip(provider, req, opts),
    fields(kind = "provider", provider = provider.name())
)]
async fn run_streaming_turn(
    provider: &dyn Provider,
    req: ChatRequest,
    opts: &LoopOptions,
) -> Result<StreamingTurn, LoopError> {
    let ring = origin_stream::Ring::with_capacity(256 * 1024);
    let drain_sub = ring.subscribe();
    if let Some(tx) = &opts.relay_tx {
        let relay_sub = ring.subscribe();
        let _ = tx.send(relay_sub).await;
    }
    let ring_for_drive = ring.clone();
    let drive = async move {
        let outcome = provider.chat_stream(req, &ring_for_drive).await;
        ring_for_drive.close();
        outcome
    };
    let drain = drain_subscriber_into_response(drain_sub, opts.cas.clone());
    let (drive_res, turn_res) = tokio::join!(drive, drain);
    drive_res?;
    turn_res
}

/// Decode a `ToolUseStart` payload into `(index, id, name)`.
/// Layout: 4-byte LE index + `id` bytes + `\0` + `name` bytes.
fn decode_tool_use_start(payload: &[u8]) -> Option<(u32, &str, &str)> {
    if payload.len() < 5 {
        return None;
    }
    let index = u32::from_le_bytes(payload[..4].try_into().ok()?);
    let rest = &payload[4..];
    let sep = rest.iter().position(|&b| b == 0)?;
    let id = std::str::from_utf8(&rest[..sep]).ok()?;
    let name = std::str::from_utf8(&rest[sep + 1..]).ok()?;
    Some((index, id, name))
}

/// Decode a `Usage` token payload (4× BE u32 = 16 bytes) into [`origin_provider::Usage`].
/// Returns `None` on any size mismatch so the caller cleanly skips malformed
/// payloads instead of panicking. Replaces a string of `try_into().expect("4 bytes")`
/// calls whose safety was load-bearing on the outer `p.len() == 16` guard.
fn decode_usage_payload(p: &[u8]) -> Option<origin_provider::Usage> {
    // The single fallible step: assert the whole payload is exactly 16 bytes.
    // After that, the four 4-byte sub-arrays come from `let-else` slice
    // conversions that re-check locally — no `expect` and no panic risk if
    // the outer length guard is ever refactored away.
    let arr: &[u8; 16] = p.try_into().ok()?;
    let Ok(a) = <[u8; 4]>::try_from(&arr[0..4]) else {
        return None;
    };
    let Ok(b) = <[u8; 4]>::try_from(&arr[4..8]) else {
        return None;
    };
    let Ok(c) = <[u8; 4]>::try_from(&arr[8..12]) else {
        return None;
    };
    let Ok(d) = <[u8; 4]>::try_from(&arr[12..16]) else {
        return None;
    };
    Some(origin_provider::Usage {
        input_tokens: u32::from_be_bytes(a),
        output_tokens: u32::from_be_bytes(b),
        cache_read_input_tokens: u32::from_be_bytes(c),
        cache_creation_input_tokens: u32::from_be_bytes(d),
    })
}

/// Try to speculatively spawn a pure tool when the first `Field` event fires.
/// Called at most once per `tool_use_id`. Returns `true` if a task was spawned.
fn try_speculative_spawn(
    tool_use_id: &str,
    tool_names: &HashMap<String, String>,
    tool_input_bufs: &HashMap<String, Vec<u8>>,
    registry: &mut SpeculativeRegistry,
    cas: Option<Arc<Store>>,
) -> bool {
    let Some(name) = tool_names.get(tool_use_id) else {
        return false;
    };
    let Some(meta) = registry_iter().find(|m| m.name == *name) else {
        return false;
    };
    if !matches!(meta.side_effects, SideEffects::Pure) {
        return false;
    }
    // Try to parse the accumulated bytes as a complete JSON object. For
    // single-field tools (Read, Glob) this succeeds at the first `Field`
    // event because the value's closing quote is also the start of the
    // outer `}`. For multi-field tools (Grep with `pattern` + `root`) the
    // first attempt may fail because only one field has arrived — we'll
    // retry on the next Field event when more bytes have accumulated.
    let buf = tool_input_bufs
        .get(tool_use_id)
        .map_or(&[] as &[u8], Vec::as_slice);
    if let Ok(args) = serde_json::from_slice::<Value>(buf) {
        registry.spawn(tool_use_id.to_owned(), meta, args, cas);
        return true;
    }
    false
}

#[allow(clippy::too_many_lines)] // streaming state-machine; extracting sub-functions would require extra allocation
async fn drain_subscriber_into_response(
    mut sub: origin_stream::Subscriber,
    cas: Option<Arc<Store>>,
) -> Result<StreamingTurn, LoopError> {
    let mut text = String::new();
    let mut usage = origin_provider::Usage::default();
    let mut blocks: Vec<Block> = Vec::new();

    let mut parsers: HashMap<String, ToolUseParser> = HashMap::new();
    let mut tool_input_bufs: HashMap<String, Vec<u8>> = HashMap::new();
    let mut tool_input_order: Vec<String> = Vec::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut index_to_id: HashMap<u32, String> = HashMap::new();
    let mut registry = SpeculativeRegistry::default();
    let mut speculative_spawned: HashSet<String> = HashSet::new();

    while let Some(ev) = sub
        .next()
        .await
        .map_err(|e| LoopError::ToolFailure(e.to_string()))?
    {
        match ev.kind() {
            origin_stream::TokenKind::TextDelta => {
                text.push_str(&String::from_utf8_lossy(ev.payload()));
            }
            origin_stream::TokenKind::ToolUseStart => {
                if let Some((index, id, name)) = decode_tool_use_start(ev.payload()) {
                    let mut parser = ToolUseParser::new();
                    parser.begin_tool_use(name);
                    parsers.insert(id.to_owned(), parser);
                    tool_names.insert(id.to_owned(), name.to_owned());
                    tool_input_bufs.insert(id.to_owned(), Vec::new());
                    if !tool_input_order.contains(&id.to_owned()) {
                        tool_input_order.push(id.to_owned());
                    }
                    index_to_id.insert(index, id.to_owned());
                } else {
                    tracing::warn!(bytes = ev.payload().len(), "malformed ToolUseStart payload");
                }
            }
            origin_stream::TokenKind::ToolUseDelta => {
                let payload = ev.payload();
                // Decode the 4-byte LE index locally without `expect` so any
                // future refactor that weakens an outer length guard cannot
                // turn this into a daemon-wide panic. A payload shorter than
                // 4 bytes is silently skipped (same behaviour as before).
                let Some(idx_slice) = payload.get(..4) else {
                    continue;
                };
                let Ok(idx_bytes) = <[u8; 4]>::try_from(idx_slice) else {
                    continue;
                };
                let index = u32::from_le_bytes(idx_bytes);
                let json_bytes = &payload[4..];
                if let Some(id) = index_to_id.get(&index) {
                    let id_owned = id.clone();
                    if let Some(buf) = tool_input_bufs.get_mut(&id_owned) {
                        buf.extend_from_slice(json_bytes);
                    } else {
                        // Invariant violation: `index_to_id` resolved but the
                        // per-id input buffer is missing. Should never happen
                        // unless `ToolUseStart` and these maps drift apart.
                        tracing::warn!(
                            index,
                            tool_use_id = %id_owned,
                            bytes = json_bytes.len(),
                            "ToolUseDelta: tool_input_bufs missing entry for known id; dropping bytes"
                        );
                    }
                    if let Some(parser) = parsers.get_mut(&id_owned) {
                        let events = parser.feed(json_bytes);
                        if !speculative_spawned.contains(&id_owned)
                            && events.iter().any(|e| matches!(e, ToolUseDelta::Field { .. }))
                            && try_speculative_spawn(
                                &id_owned,
                                &tool_names,
                                &tool_input_bufs,
                                &mut registry,
                                cas.clone(),
                            )
                        {
                            speculative_spawned.insert(id_owned);
                        }
                    } else {
                        // Same invariant: parsers map should mirror index_to_id.
                        tracing::warn!(
                            index,
                            tool_use_id = %id_owned,
                            "ToolUseDelta: parsers missing entry for known id; speculative dispatch skipped"
                        );
                    }
                } else {
                    // Orphan delta: index was never opened (likely because a
                    // prior `ToolUseStart` payload was malformed and warned
                    // out at the decode site above). Log it so the dropped
                    // tool input is at least observable.
                    tracing::warn!(
                        index,
                        bytes = json_bytes.len(),
                        "ToolUseDelta for unknown index; dropping bytes (no matching ToolUseStart)"
                    );
                }
            }
            origin_stream::TokenKind::Usage => {
                if let Some(parsed) = decode_usage_payload(ev.payload()) {
                    usage = parsed;
                }
            }
            origin_stream::TokenKind::TurnEnd => break,
            origin_stream::TokenKind::ThinkingDelta => {}
        }
    }

    if !text.is_empty() {
        blocks.push(Block::Text {
            text,
            cache_marker: None,
        });
    }
    for id in &tool_input_order {
        let Some(buf) = tool_input_bufs.get(id) else {
            continue;
        };
        let name = tool_names.get(id).cloned().unwrap_or_default();
        blocks.push(Block::ToolUse {
            id: id.clone(),
            name,
            input_json: buf.clone(),
            cache_marker: None,
        });
    }
    let assistant = Message {
        role: Role::Assistant,
        blocks,
    };
    Ok(StreamingTurn {
        response: origin_provider::ChatResponse { assistant, usage },
        speculative: registry,
    })
}

#[cfg(test)]
mod dispatch_table_tests {
    use super::*;
    use origin_tools::dispatch::{MemoryHandle, MemoryToolError, SearchHit};
    use origin_tools::registry_iter;
    use std::sync::Mutex;

    /// Build a fresh in-memory-ish `CodeGraphIndex` backed by a tempdir for CAS
    /// and an in-memory (`:memory:`) `SQLite` database. Returns the index wrapped
    /// in `Arc<tokio::sync::Mutex<...>>` as required by `LoopOptions`.
    fn make_empty_code_graph() -> (
        Arc<tokio::sync::Mutex<origin_codegraph::index::CodeGraphIndex>>,
        tempfile::TempDir,
    ) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cas_root = tmp.path().join("cas");
        let cas = origin_cas::Store::open(origin_cas::StoreConfig {
            root: cas_root,
            hot_capacity: 64,
            warm_pack_target_bytes: 1 << 20,
            cold_zstd_level: 3,
        })
        .expect("open cas");
        // Use the temp db path rather than :memory: so migrations run via refinery.
        let db_path = tmp.path().join("origin.db");
        let sql = origin_store::Store::open(&db_path).expect("open sqlite");
        let idx = origin_codegraph::index::CodeGraphIndex::new(cas, sql);
        (Arc::new(tokio::sync::Mutex::new(idx)), tmp)
    }

    /// Every tool advertised to the model via `tools_schema = registry_iter().map(...)`
    /// MUST be recognized by `dispatch_tool`. An `UnknownTool` error means the
    /// model received a tool name it can pick, then got told "I don't know that
    /// tool" — which is misleading. Tools whose subsystems are not yet wired
    /// should return `ToolFailure(<reason>)`, NOT `UnknownTool`.
    #[tokio::test]
    async fn dispatch_tool_recognizes_every_registered_tool() {
        let empty = serde_json::Value::Object(serde_json::Map::new());
        let mut unrecognized: Vec<String> = Vec::new();
        for meta in registry_iter() {
            let result = dispatch_tool(meta, &empty, None, None, None, None, None).await;
            if let Err(LoopError::UnknownTool(name)) = &result {
                unrecognized.push(name.clone());
            }
        }
        assert!(
            unrecognized.is_empty(),
            "tools registered in the inventory but not handled by dispatch_tool: {unrecognized:?}"
        );
    }

    /// `graph_explain` is the only non-`Recall` tool wired here with a real
    /// implementation (no missing subsystem). Verify it produces the expected
    /// English gloss for each Query variant.
    #[tokio::test]
    async fn graph_explain_returns_real_nl_gloss() {
        let meta = registry_iter()
            .find(|m| m.name == "graph_explain")
            .expect("graph_explain registered");
        let args = serde_json::json!({"kind": "communities"});
        let out = dispatch_tool(meta, &args, None, None, None, None, None)
            .await
            .expect("communities dispatch");
        assert_eq!(out, "all detected communities");

        let args = serde_json::json!({
            "kind": "recent_changes",
            "args": {"since_ms": 1_700_000_000_000_i64}
        });
        let out = dispatch_tool(meta, &args, None, None, None, None, None)
            .await
            .expect("recent_changes dispatch");
        assert!(out.contains("1700000000000"), "got: {out}");

        // Unknown kind surfaces as BadArgs, not ToolFailure or UnknownTool.
        let args = serde_json::json!({"kind": "bogus"});
        let err = dispatch_tool(meta, &args, None, None, None, None, None)
            .await
            .expect_err("bogus must fail");
        assert!(matches!(err, LoopError::BadArgs(_)));
    }

    /// The stub arms return `ToolFailure` with messages naming the missing
    /// subsystem — never `UnknownTool`. Regression guard for accidental
    /// reversion to the silent-fall-through.
    #[allow(clippy::panic)]
    #[tokio::test]
    async fn stub_arms_return_toolfailure_not_unknowntool() {
        // After Subsystems A (graph_* + ask), B (mem_*) merged, only `Task`
        // remains a stub. The behavior contract is unchanged: missing
        // subsystem → ToolFailure with subsystem-naming message.
        // mem_* / graph_* / ask still surface a `ToolFailure` when their
        // handle is None (covered in their dedicated tests below); calling
        // them here with all-None would test the same path, so we keep this
        // suite narrow to the remaining literal stubs.
        let names = ["Task"];
        let args = serde_json::Value::Object(serde_json::Map::new());
        for name in names {
            let meta = registry_iter()
                .find(|m| m.name == name)
                .unwrap_or_else(|| panic!("{name} not registered"));
            let err = dispatch_tool(meta, &args, None, None, None, None, None)
                .await
                .expect_err(name);
            match err {
                LoopError::ToolFailure(msg) => {
                    assert!(
                        msg.contains("not yet wired") || msg.contains("subsystem"),
                        "{name}: ToolFailure message must name the missing subsystem; got `{msg}`"
                    );
                }
                LoopError::UnknownTool(_) => panic!("{name}: regressed to UnknownTool"),
                other => panic!("{name}: unexpected error variant {other:?}"),
            }
        }
    }

    // ── Subsystem A tests: graph_* + ask (with CodeGraphIndex) ────────────────

    /// Dispatch `graph_query` with `kind=communities` against an empty index.
    /// Post-P7.8 Communities returns `QueryResult::Partitions` (empty list
    /// when the edge table has no rows), which serializes with a
    /// `partitions` field.
    #[tokio::test]
    async fn graph_query_runs_against_empty_index_returns_empty_result() {
        let (code_graph, _tmp) = make_empty_code_graph();
        let meta = registry_iter()
            .find(|m| m.name == "graph_query")
            .expect("graph_query registered");
        let args = serde_json::json!({"kind": "communities"});
        let out = dispatch_tool(meta, &args, None, Some(&code_graph), None, None, None)
            .await
            .expect("graph_query dispatch");
        // Empty edge table yields an empty Partitions list.
        assert_eq!(
            out, r#"{"partitions":[]}"#,
            "expected empty partitions JSON, got: {out}"
        );
    }

    /// Dispatch `ask` with a code-flavored query against an empty index +
    /// `NullMemRouter`. The classifier routes "what calls foo" to `Route::Code`
    /// so `result.route` must serialize as `"code"`.
    #[tokio::test]
    async fn ask_classifies_pure_code_query() {
        let (code_graph, _tmp) = make_empty_code_graph();
        let meta = registry_iter().find(|m| m.name == "ask").expect("ask registered");
        let args = serde_json::json!({"query": "what calls foo"});
        let null_router = origin_codegraph::ask::NullMemRouter;
        let out = dispatch_tool(
            meta,
            &args,
            None,
            Some(&code_graph),
            Some(&null_router as &dyn origin_codegraph::ask::MemRouter),
            None,
            None,
        )
        .await
        .expect("ask dispatch");
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        let route = parsed["route"].as_str().expect("route field");
        assert_eq!(route, "code", "expected route=code, got: {out}");
    }

    /// Dispatch `graph_rebuild` with an empty paths array. The report must show
    /// `paths_seen = 0` and no errors.
    #[tokio::test]
    async fn graph_rebuild_with_empty_paths_returns_zero_report() {
        let (code_graph, _tmp) = make_empty_code_graph();
        let meta = registry_iter()
            .find(|m| m.name == "graph_rebuild")
            .expect("graph_rebuild registered");
        let args = serde_json::json!({"paths": []});
        let out = dispatch_tool(meta, &args, None, Some(&code_graph), None, None, None)
            .await
            .expect("graph_rebuild dispatch");
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        let paths_seen = parsed["paths_seen"].as_u64().expect("paths_seen field");
        assert_eq!(paths_seen, 0, "expected 0 paths_seen, got: {out}");
        let errors = parsed["errors"].as_array().expect("errors field");
        assert!(errors.is_empty(), "expected no errors, got: {out}");
    }

    // ── Subsystem B tests: mem_* (with MemoryHandle) ──────────────────────────

    /// A minimal in-memory `MemoryHandle` implementation for testing.
    /// Uses a `Mutex<Vec<_>>` so it is `Send + Sync` and requires no external deps.
    #[derive(Debug)]
    struct StubMemoryHandle {
        entries: Mutex<Vec<(String, String, Vec<String>)>>, // (id, body, tags)
    }

    impl StubMemoryHandle {
        const fn new() -> Self {
            Self {
                entries: Mutex::new(Vec::new()),
            }
        }
    }

    impl MemoryHandle for StubMemoryHandle {
        fn search(&self, query: &str, k: usize, _fresh: bool) -> Result<Vec<SearchHit>, MemoryToolError> {
            let q_lower = query.to_lowercase();
            let hits: Vec<SearchHit> = {
                let entries = self.entries.lock().expect("lock");
                entries
                    .iter()
                    .filter(|(_, body, _)| body.to_lowercase().contains(&q_lower))
                    .take(k)
                    .map(|(id, body, tags)| SearchHit {
                        id: id.clone(),
                        preview: body.chars().take(128).collect(),
                        score: 1.0,
                        age_days: 0.0,
                        tags: tags.clone(),
                    })
                    .collect()
            };
            Ok(hits)
        }

        fn save(&self, body: &str, tags: &[String]) -> Result<String, MemoryToolError> {
            let id = format!("stub-{}", ulid::Ulid::new());
            self.entries
                .lock()
                .expect("lock")
                .push((id.clone(), body.to_string(), tags.to_vec()));
            Ok(id)
        }

        fn forget(&self, id: &str) -> Result<(), MemoryToolError> {
            let mut entries = self.entries.lock().expect("lock");
            let before = entries.len();
            entries.retain(|(eid, _, _)| eid != id);
            if entries.len() < before {
                Ok(())
            } else {
                Err(MemoryToolError::BadId(id.to_string()))
            }
        }
    }

    /// `mem_search` with `memory_handle = None` must return `ToolFailure` containing
    /// "subsystem" — preserving the no-handle behavior.
    #[allow(clippy::panic)]
    #[tokio::test]
    async fn mem_search_without_handle_returns_toolfailure() {
        let meta = registry_iter()
            .find(|m| m.name == "mem_search")
            .expect("mem_search registered");
        let args = serde_json::json!({"query": "anything"});
        let err = dispatch_tool(meta, &args, None, None, None, None, None)
            .await
            .expect_err("must fail without handle");
        match err {
            LoopError::ToolFailure(msg) => {
                assert!(
                    msg.contains("subsystem"),
                    "ToolFailure must mention subsystem; got `{msg}`"
                );
            }
            other => panic!("expected ToolFailure, got {other:?}"),
        }
    }

    /// Wire a `StubMemoryHandle` through dispatch, save a memory via `mem_save`,
    /// then confirm `mem_search` returns the saved item.
    #[tokio::test]
    async fn mem_save_round_trips_via_handle() {
        let handle = StubMemoryHandle::new();

        let save_meta = registry_iter()
            .find(|m| m.name == "mem_save")
            .expect("mem_save registered");
        let save_args = serde_json::json!({
            "body": "the quick brown fox",
            "tags": ["test", "roundtrip"]
        });
        let save_out = dispatch_tool(save_meta, &save_args, None, None, None, Some(&handle), None)
            .await
            .expect("mem_save must succeed");
        let save_json: serde_json::Value =
            serde_json::from_str(&save_out).expect("mem_save output must be valid JSON");
        assert!(
            save_json.get("id").and_then(|v| v.as_str()).is_some(),
            "mem_save must return {{\"id\":\"...\"}}; got `{save_out}`"
        );

        let search_meta = registry_iter()
            .find(|m| m.name == "mem_search")
            .expect("mem_search registered");
        let search_args = serde_json::json!({"query": "quick brown", "k": 5});
        let search_out = dispatch_tool(search_meta, &search_args, None, None, None, Some(&handle), None)
            .await
            .expect("mem_search must succeed");
        let hits: serde_json::Value =
            serde_json::from_str(&search_out).expect("mem_search output must be valid JSON");
        let arr = hits.as_array().expect("mem_search must return an array");
        assert!(
            !arr.is_empty(),
            "mem_search must find the saved entry; got empty array"
        );
        let first = &arr[0];
        assert!(
            first["preview"]
                .as_str()
                .map_or(false, |p| p.contains("quick brown")),
            "hit preview must contain the saved body; got {first}"
        );
    }

    // ── Subsystem C tests: Task (with swarm Coordinator) ──────────────────────

    /// When `coordinator` is `None`, `Task` must return `ToolFailure` (not
    /// `UnknownTool`). Regression guard for the subsystem-not-configured path.
    #[allow(clippy::panic)]
    #[tokio::test]
    async fn task_without_coordinator_returns_toolfailure() {
        let meta = registry_iter()
            .find(|m| m.name == "Task")
            .expect("Task registered");
        let args = serde_json::json!({
            "goal": "do something",
            "allowed_tools": []
        });
        let err = dispatch_tool(meta, &args, None, None, None, None, None)
            .await
            .expect_err("Task without coordinator must fail");
        match err {
            LoopError::ToolFailure(msg) => {
                assert!(
                    msg.contains("subsystem"),
                    "Task ToolFailure message must mention subsystem; got `{msg}`"
                );
            }
            LoopError::UnknownTool(_) => panic!("Task regressed to UnknownTool"),
            other => panic!("unexpected error variant {other:?}"),
        }
    }

    /// When a real `Coordinator` (backed by an in-memory `Plan` + `PlanStore`
    /// over in-memory `CasStore` + `SqlStore`) is threaded through, `Task` must
    /// return a valid `TaskOutput` JSON. The default noop worker always completes
    /// successfully, so the round-trip asserts shape, not semantic content.
    #[tokio::test]
    async fn task_with_coordinator_spawns_noop_worker() {
        use std::sync::Arc;

        // Build in-memory backing stores.
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("plan.db");
        let sql = Arc::new(origin_store::Store::open(db_path.to_str().expect("utf8")).expect("sql open"));
        let cas_root = tmp.path().join("cas");
        let cas = Arc::new(
            origin_cas::Store::open(origin_cas::StoreConfig {
                root: cas_root,
                hot_capacity: 16,
                warm_pack_target_bytes: 1024 * 1024,
                cold_zstd_level: 1,
            })
            .expect("cas open"),
        );

        // Construct Plan / PlanStore / PlanHandle / Coordinator.
        let plan = Arc::new(tokio::sync::Mutex::new(origin_plan::Plan::new()));
        let plan_store = Arc::new(
            origin_plan::PlanStore::open(Arc::clone(&sql), Arc::clone(&cas)).expect("plan store open"),
        );
        let plan_handle = origin_swarm::PlanHandle::new(plan, plan_store);
        let coordinator = Arc::new(origin_swarm::Coordinator::new(plan_handle, "test-ring"));

        let meta = registry_iter()
            .find(|m| m.name == "Task")
            .expect("Task registered");
        let args = serde_json::json!({
            "goal": "noop integration test",
            "allowed_tools": []
        });

        let out = dispatch_tool(meta, &args, None, None, None, None, Some(coordinator.as_ref()))
            .await
            .expect("Task with coordinator must succeed");

        // Assert the output is valid JSON with the expected shape.
        let v: serde_json::Value = serde_json::from_str(&out).expect("TaskOutput must be valid JSON");
        assert!(v.get("status").is_some(), "TaskOutput must have `status`");
        assert!(v.get("summary").is_some(), "TaskOutput must have `summary`");
        assert!(
            v.get("files_touched").is_some(),
            "TaskOutput must have `files_touched`"
        );
        assert!(v.get("follow_ups").is_some(), "TaskOutput must have `follow_ups`");
    }

    /// Defensive guard against the previous `try_into().expect("4 bytes")`
    /// pattern in the `Usage` decode path. After the refactor to
    /// [`decode_usage_payload`], a payload shorter than 16 bytes (or longer)
    /// must return `None` rather than panicking — matching the pre-fix
    /// outer `if p.len() == 16` skip semantics.
    #[test]
    fn decode_usage_payload_returns_none_on_size_mismatch() {
        // Too short: pre-fix `if p.len() == 16` would have skipped this; the
        // helper must also return None (no panic from the inner `expect`).
        assert!(decode_usage_payload(&[]).is_none());
        assert!(decode_usage_payload(&[0; 3]).is_none());
        assert!(decode_usage_payload(&[0; 15]).is_none());
        // Too long: previously fell through the `==` guard silently; the
        // helper must also return None for consistency.
        assert!(decode_usage_payload(&[0; 17]).is_none());
        assert!(decode_usage_payload(&[0; 32]).is_none());
    }

    /// Round-trip a 16-byte BE-encoded payload through [`decode_usage_payload`]
    /// to lock down the byte layout: 4× u32 BE in field order
    /// (input, output, cache_read, cache_creation).
    #[test]
    fn decode_usage_payload_parses_canonical_16_byte_layout() {
        let mut p = [0u8; 16];
        p[0..4].copy_from_slice(&1_111_u32.to_be_bytes());
        p[4..8].copy_from_slice(&2_222_u32.to_be_bytes());
        p[8..12].copy_from_slice(&3_333_u32.to_be_bytes());
        p[12..16].copy_from_slice(&4_444_u32.to_be_bytes());
        let usage = decode_usage_payload(&p).expect("valid 16-byte payload");
        assert_eq!(usage.input_tokens, 1_111);
        assert_eq!(usage.output_tokens, 2_222);
        assert_eq!(usage.cache_read_input_tokens, 3_333);
        assert_eq!(usage.cache_creation_input_tokens, 4_444);
    }
}
