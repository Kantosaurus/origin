//! Agent loop: prompt → provider → tool dispatch → repeat → final text.

use crate::protocol::StreamEvent;
use crate::session::Session;
use crate::session_store::SessionStore;
use crate::tool_use_parser::{ToolUseDelta, ToolUseParser};
use origin_cas::{Hash, Store};
use origin_core::types::{Block, Message, Role};
use origin_mem::{Injector, Proposer};
use origin_permission::{check, prompt::Prompter, Outcome};
use origin_provider::{ChatRequest, Provider};
use origin_runtime::{spawn_in, TaskClass};
use origin_sidecar::{ExtractDeliverer, Sidecar, SummaryDeliverer};
use origin_tools::{registry_iter, SideEffects, ToolMeta};
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
            let text = dispatch_tool(meta, &args, cas.as_deref()).await?;
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
    let recalled_system =
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

            // P6.7: optional proposer pass at turn end. Each proposal is
            // stashed on the Session and surfaced as a side-band StreamEvent
            // for the CLI to render.
            if let Some(proposer) = &opts.proposer {
                let proposals = proposer.scan(user_text, &text, &mut session.next_proposal_id);
                for p in proposals {
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
            let decision = check(meta, &preview, prompter).await;
            if decision.outcome == Outcome::Deny {
                // Drain any speculative slot to keep the registry clean.
                let _ = speculative.take(&id).await;
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
                    dispatch_tool(meta, &args, opts.cas.as_deref())
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
    skip(args, cas),
    fields(kind = "tool", tool = meta.name)
)]
async fn dispatch_tool(meta: &ToolMeta, args: &Value, cas: Option<&Store>) -> Result<String, LoopError> {
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
