//! Agent loop: prompt → provider → tool dispatch → repeat → final text.

use crate::proposal_registry::ProposalRegistry;
use crate::protocol::StreamEvent;
use crate::session::Session;
use crate::session_store::SessionStore;
use crate::tool_use_parser::{ToolUseDelta, ToolUseParser};
use origin_cas::{Hash, Store};
use origin_core::types::{Block, Message, Role};
use origin_mem::{Injector, Proposer};
use origin_permission::{check_with_skills, prompt::Prompter, Outcome};
use origin_provider::{ChatRequest, Provider};
use origin_runtime::{spawn_in, TaskClass};
use origin_sidecar::{ExtractDeliverer, Sidecar, SummaryDeliverer};
use origin_skills::SkillRegistry;
use crate::skill_catalog::SkillCatalog;
use origin_tools::{registry_iter, SideEffects, ToolMeta};
use origin_tools::dispatch::MemoryHandle;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use thiserror::Error;
use tokio::task::spawn_blocking as sb;
use tokio::task::JoinHandle;

#[derive(Clone)]
pub struct LoopOptions {
    pub max_turns: u32,
    pub cas: Option<Arc<Store>>,
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
    /// Optional memory-subsystem handle. When `Some`, `mem_search`,
    /// `mem_save`, and `mem_forget` dispatch to the live `MemoryStore` /
    /// HNSW index. When `None`, those tools return
    /// `ToolFailure("memory subsystem not configured")`.
    pub memory_handle: Option<Arc<dyn MemoryHandle>>,
}

impl Default for LoopOptions {
    fn default() -> Self {
        Self {
            max_turns: 25,
            cas: None,
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
            memory_handle: None,
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
            // Speculative tasks are spawned for Pure tools only. Memory tools
            // that are Pure (e.g. mem_search) don't get speculative dispatch
            // because they need the memory handle which cannot be moved into
            // the spawned task without an Arc clone. Since speculative spawning
            // for mem_search is an optimisation-only path, we pass None here
            // and let the main dispatch path handle it with the real handle.
            let text = dispatch_tool(meta, &args, cas.as_deref(), None).await?;
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
    session.push(Message::new(Role::User).with_block(Block::text(user_text)));

    let tools_schema = registry_iter()
        .map(|m| origin_provider::ToolSchema {
            name: m.name.to_string(),
            description: m.description.to_string(),
            input_schema_json: m.input_schema.to_string(),
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
            if cat.is_empty() {
                String::new()
            } else {
                let active_names: std::collections::HashSet<String> = opts
                    .skills
                    .as_ref()
                    .map(|reg| reg.iter_active().map(|s| s.name.clone()).collect())
                    .unwrap_or_default();
                let mut out = String::from(
                    "Available skills (activate via `/<name>`, deactivate via `/-<name>`):\n",
                );
                for s in cat.iter() {
                    let marker = if active_names.contains(&s.front.name) {
                        "*"
                    } else {
                        "-"
                    };
                    use std::fmt::Write as _;
                    let _ = writeln!(out, "  {marker} {}: {}", s.front.name, s.front.description);
                }
                out
            }
        })
        .unwrap_or_default();

    // Concatenate: catalog first (so it's stable across recall variation),
    // then recall context.
    let recalled_system = if catalog_block.is_empty() {
        recall_block
    } else if recall_block.is_empty() {
        catalog_block
    } else {
        format!("{catalog_block}\n{recall_block}")
    };

    for turn in 1..=opts.max_turns {
        let req = ChatRequest {
            system: recalled_system.clone(),
            messages: session.snapshot(),
            model: session.model.clone(),
            tools: tools_schema.clone(),
        };

        let (resp, mut speculative) = if opts.streaming_disabled {
            let r = provider.chat(req).await?;
            (r, SpeculativeRegistry::default())
        } else {
            let st = run_streaming_turn(provider, req, opts).await?;
            (st.response, st.speculative)
        };

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
            });
        }

        // Dispatch each tool_use sequentially.
        let mut tool_results: Vec<Block> = Vec::with_capacity(tool_uses.len());
        for (id, name, input_bytes) in tool_uses {
            let meta = registry_iter()
                .find(|m| m.name == name)
                .ok_or_else(|| LoopError::UnknownTool(name.clone()))?;
            let args: Value =
                serde_json::from_slice(&input_bytes).map_err(|e| LoopError::BadArgs(e.to_string()))?;
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
            // Falls back to an empty static `SkillRegistry` when none is wired,
            // so the call path is identical; an empty stack's mask is `None`,
            // which makes `check_with_skills` short-circuit to plain `check`.
            static EMPTY_SKILLS: SkillRegistry = SkillRegistry::new();
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
                    pre?
                } else {
                    dispatch_tool(meta, &args, opts.cas.as_deref(), opts.memory_handle.as_deref())
                        .await?
                        .into_bytes()
                }
            };

            let block = if let Some(cas) = opts.cas.as_ref() {
                let h: Hash = cas
                    .put(&result_bytes)
                    .map_err(|e| LoopError::ToolFailure(e.to_string()))?;

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
    }
    Err(LoopError::MaxTurns(opts.max_turns))
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
    skip(args, cas, memory),
    fields(kind = "tool", tool = meta.name)
)]
async fn dispatch_tool(
    meta: &ToolMeta,
    args: &Value,
    cas: Option<&Store>,
    memory: Option<&dyn MemoryHandle>,
) -> Result<String, LoopError> {
    match meta.name {
        "Read" => {
            let path = args
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Read: missing `path`".into()))?;
            origin_tools::builtins::read::read_tool(path).map_err(|e| LoopError::ToolFailure(e.to_string()))
        }
        "Glob" => {
            let pat = args
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Glob: missing `pattern`".into()))?;
            let hits = origin_tools::builtins::glob_tool::glob_tool(pat).map_err(LoopError::ToolFailure)?;
            Ok(hits.join("\n"))
        }
        "Grep" => {
            let pat = args
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Grep: missing `pattern`".into()))?;
            let root = args
                .get("root")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Grep: missing `root`".into()))?;
            let hits =
                origin_tools::builtins::grep_tool::grep_tool(pat, root).map_err(LoopError::ToolFailure)?;
            Ok(hits.join("\n"))
        }
        "Edit" => {
            let path = args
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Edit: missing `path`".into()))?;
            let old = args
                .get("old_string")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Edit: missing `old_string`".into()))?;
            let new = args
                .get("new_string")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Edit: missing `new_string`".into()))?;
            origin_tools::builtins::edit::edit_tool(path, old, new)
                .map(|()| "edit ok".to_string())
                .map_err(LoopError::ToolFailure)
        }
        "Bash" => {
            let cmd = args
                .get("command")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Bash: missing `command`".into()))?;
            let out = origin_tools::builtins::bash::bash_tool(cmd)
                .await
                .map_err(LoopError::ToolFailure)?;
            Ok(format!(
                "exit_code: {}\nstdout:\n{}\nstderr:\n{}",
                out.exit_code, out.stdout, out.stderr
            ))
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
        // These four require an `Arc<origin_codegraph::index::CodeGraphIndex>`
        // threaded through `LoopOptions`. The daemon currently constructs
        // neither the index nor the SQL pool side it needs, so dispatch returns
        // a clear `ToolFailure` rather than `UnknownTool` — the tool IS in the
        // catalog the model receives, the subsystem behind it just isn't wired.
        "graph_query" => Err(LoopError::ToolFailure(
            "graph_query: code-graph subsystem not yet wired. Thread a CodeGraphIndex \
             through LoopOptions and extend dispatch_tool to consume it."
                .into(),
        )),
        "graph_path" => Err(LoopError::ToolFailure(
            "graph_path: code-graph subsystem not yet wired (see graph_query notes).".into(),
        )),
        "graph_summarize" => Err(LoopError::ToolFailure(
            "graph_summarize: code-graph subsystem not yet wired (see graph_query notes).".into(),
        )),
        "graph_rebuild" => Err(LoopError::ToolFailure(
            "graph_rebuild: code-graph subsystem not yet wired. The free function \
             `agent::rebuild_codegraph` exists but no IPC verb or background hook \
             routes to it; this dispatch arm activates once both are present."
                .into(),
        )),
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
                hex::decode_to_slice(s, &mut buf).map_err(|e| {
                    LoopError::BadArgs(format!("graph_explain.{field}: bad hex: {e}"))
                })?;
                Ok(EntityId(buf))
            };
            let q = match kind {
                "path" => Query::Path {
                    from: parse_id(&q_args["from"], "args.from")?,
                    to: parse_id(&q_args["to"], "args.to")?,
                    max_hops: usize::try_from(q_args["max_hops"].as_u64().unwrap_or(8))
                        .unwrap_or(usize::MAX),
                },
                "neighbors" => Query::Neighbors {
                    node: parse_id(&q_args["node"], "args.node")?,
                    depth: usize::try_from(q_args["depth"].as_u64().unwrap_or(1))
                        .unwrap_or(usize::MAX),
                },
                "communities" => Query::Communities,
                "god_nodes" => Query::GodNodes {
                    top_per_partition: usize::try_from(
                        q_args["top_per_partition"].as_u64().unwrap_or(3),
                    )
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
        // Needs both a CodeGraphIndex (for code routing) and a MemRouter.
        // `NullMemRouter` is available as a stub but the full router needs
        // wiring against the daemon's mem index.
        "ask" => Err(LoopError::ToolFailure(
            "ask: free-text router not yet wired. Needs CodeGraphIndex + MemRouter \
             threaded through LoopOptions."
                .into(),
        )),
        // ── Task ──
        // Needs an `origin_swarm::Coordinator` constructed against a PlanHandle.
        // The daemon has a PlanBus but no Coordinator instance yet.
        "Task" => Err(LoopError::ToolFailure(
            "Task: swarm subsystem not yet wired. Construct an origin_swarm::Coordinator \
             at daemon startup and thread it through LoopOptions."
                .into(),
        )),
        other => Err(LoopError::UnknownTool(other.into())),
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

/// Decode a `ToolUseStart` payload into `(id, name)`.
/// Layout: `id` bytes + `\0` + `name` bytes.
fn decode_tool_use_start(payload: &[u8]) -> Option<(&str, &str)> {
    let sep = payload.iter().position(|&b| b == 0)?;
    let id = std::str::from_utf8(&payload[..sep]).ok()?;
    let name = std::str::from_utf8(&payload[sep + 1..]).ok()?;
    Some((id, name))
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

    // P3.4: per-id incremental JSON parsers for active tool_use blocks.
    // DONE_WITH_CONCERNS: routing deltas to the "most recent" parser is a
    // simplification — Anthropic can interleave deltas for concurrent
    // tool_use blocks by index. Full index-based routing is deferred to a
    // follow-up that adds an `index` field to the ToolUseDelta payload.
    let mut parsers: HashMap<String, ToolUseParser> = HashMap::new();
    let mut active_id: Option<String> = None;
    let mut tool_input_bufs: HashMap<String, Vec<u8>> = HashMap::new();
    let mut tool_input_order: Vec<String> = Vec::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();
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
                if let Some((id, name)) = decode_tool_use_start(ev.payload()) {
                    let mut parser = ToolUseParser::new();
                    parser.begin_tool_use(name);
                    parsers.insert(id.to_owned(), parser);
                    tool_names.insert(id.to_owned(), name.to_owned());
                    tool_input_bufs.insert(id.to_owned(), Vec::new());
                    if !tool_input_order.contains(&id.to_owned()) {
                        tool_input_order.push(id.to_owned());
                    }
                    active_id = Some(id.to_owned());
                } else {
                    tracing::warn!(
                        bytes = ev.payload().len(),
                        "malformed ToolUseStart payload; \
                         routing for subsequent ToolUseDelta events may be incorrect"
                    );
                }
            }
            origin_stream::TokenKind::ToolUseDelta => {
                if let Some(id) = &active_id {
                    if let Some(buf) = tool_input_bufs.get_mut(id) {
                        buf.extend_from_slice(ev.payload());
                    }
                    let id_owned = id.clone();
                    if let Some(parser) = parsers.get_mut(&id_owned) {
                        let events = parser.feed(ev.payload());
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
                    }
                }
            }
            origin_stream::TokenKind::Usage => {
                let p = ev.payload();
                if p.len() == 16 {
                    usage = origin_provider::Usage {
                        input_tokens: u32::from_be_bytes(p[0..4].try_into().expect("4 bytes")),
                        output_tokens: u32::from_be_bytes(p[4..8].try_into().expect("4 bytes")),
                        cache_read_input_tokens: u32::from_be_bytes(p[8..12].try_into().expect("4 bytes")),
                        cache_creation_input_tokens: u32::from_be_bytes(
                            p[12..16].try_into().expect("4 bytes"),
                        ),
                    };
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
            let result = dispatch_tool(meta, &empty, None, None).await;
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
        let out = dispatch_tool(meta, &args, None, None).await.expect("communities dispatch");
        assert_eq!(out, "all detected communities");

        let args = serde_json::json!({
            "kind": "recent_changes",
            "args": {"since_ms": 1_700_000_000_000_i64}
        });
        let out = dispatch_tool(meta, &args, None, None).await.expect("recent_changes dispatch");
        assert!(out.contains("1700000000000"), "got: {out}");

        // Unknown kind surfaces as BadArgs, not ToolFailure or UnknownTool.
        let args = serde_json::json!({"kind": "bogus"});
        let err = dispatch_tool(meta, &args, None, None).await.expect_err("bogus must fail");
        assert!(matches!(err, LoopError::BadArgs(_)));
    }

    /// The stub arms return `ToolFailure` with messages naming the missing
    /// subsystem — never `UnknownTool`. Regression guard for accidental
    /// reversion to the silent-fall-through.
    #[tokio::test]
    async fn stub_arms_return_toolfailure_not_unknowntool() {
        let names = [
            "graph_query",
            "graph_path",
            "graph_summarize",
            "graph_rebuild",
            "mem_search",
            "mem_save",
            "mem_forget",
            "ask",
            "Task",
        ];
        let args = serde_json::Value::Object(serde_json::Map::new());
        for name in names {
            let meta = registry_iter()
                .find(|m| m.name == name)
                .unwrap_or_else(|| panic!("{name} not registered"));
            let err = dispatch_tool(meta, &args, None, None).await.expect_err(name);
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

    // ── In-memory stub MemoryHandle for unit tests ────────────────────────────

    /// A minimal in-memory `MemoryHandle` implementation for testing.
    /// Uses a `Mutex<Vec<_>>` so it is `Send + Sync` and requires no external deps.
    #[derive(Debug)]
    struct StubMemoryHandle {
        entries: Mutex<Vec<(String, String, Vec<String>)>>, // (id, body, tags)
    }

    impl StubMemoryHandle {
        fn new() -> Self {
            Self {
                entries: Mutex::new(Vec::new()),
            }
        }
    }

    impl MemoryHandle for StubMemoryHandle {
        fn search(&self, query: &str, k: usize, _fresh: bool) -> Result<Vec<SearchHit>, MemoryToolError> {
            let entries = self.entries.lock().expect("lock");
            let q_lower = query.to_lowercase();
            let hits: Vec<SearchHit> = entries
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
                .collect();
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
    #[tokio::test]
    async fn mem_search_without_handle_returns_toolfailure() {
        let meta = registry_iter()
            .find(|m| m.name == "mem_search")
            .expect("mem_search registered");
        let args = serde_json::json!({"query": "anything"});
        let err = dispatch_tool(meta, &args, None, None)
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
        let save_out = dispatch_tool(save_meta, &save_args, None, Some(&handle))
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
        let search_out = dispatch_tool(search_meta, &search_args, None, Some(&handle))
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
}
