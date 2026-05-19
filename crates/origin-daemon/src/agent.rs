//! Agent loop: prompt → provider → tool dispatch → repeat → final text.

use crate::session::Session;
use crate::tool_use_parser::{ToolUseDelta, ToolUseParser};
use origin_cas::{Hash, Store};
use origin_core::types::{Block, Message, Role};
use origin_permission::{check, prompt::Prompter, Outcome};
use origin_provider::{ChatRequest, Provider};
use origin_tools::{registry_iter, SideEffects, ToolMeta};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
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
    /// `provider.chat_stream()`. Required for `tool_use` turns until
    /// incremental `tool_use` JSON parsing lands (P3.3).
    pub streaming_disabled: bool,
}

impl Default for LoopOptions {
    fn default() -> Self {
        Self {
            max_turns: 25,
            cas: None,
            relay_tx: None,
            streaming_disabled: false,
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
    fn spawn(&mut self, tool_use_id: String, meta: &'static ToolMeta, args: serde_json::Value) {
        // Side-effecting tools opt out — N2.2.
        if !matches!(meta.side_effects, SideEffects::Pure) {
            return;
        }
        let handle = tokio::spawn(async move {
            let text = dispatch_tool(meta, &args).await?;
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

    for turn in 1..=opts.max_turns {
        let req = ChatRequest {
            system: String::new(),
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

            let decision = check(meta, &preview, prompter).await;
            if decision.outcome == Outcome::Deny {
                // Discard any precomputed speculative result — the tool was denied.
                let _ = speculative.take(&id).await;
                return Err(LoopError::Denied(name.clone()));
            }

            // Try the speculative precomputed result first; fall back to
            // synchronous dispatch if the registry has no entry (e.g., the tool
            // is side-effecting, streaming was disabled, or the spawn lost).
            let result_bytes: Vec<u8> = if let Some(pre) = speculative.take(&id).await {
                pre?
            } else {
                dispatch_tool(meta, &args).await?.into_bytes()
            };

            let block = if let Some(cas) = opts.cas.as_ref() {
                let h: Hash = cas
                    .put(&result_bytes)
                    .map_err(|e| LoopError::ToolFailure(e.to_string()))?;
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

async fn dispatch_tool(meta: &ToolMeta, args: &Value) -> Result<String, LoopError> {
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
        other => Err(LoopError::UnknownTool(other.into())),
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
    let drain = drain_subscriber_into_response(drain_sub);
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
    // Re-parse the accumulated bytes accumulated so far. For single-field pure
    // tools (Read/Glob/Grep) the first Field event arrives with a complete JSON
    // value, so `from_slice` succeeds here. Multi-field partial args are
    // best-effort: if parse fails we skip the spawn.
    let buf = tool_input_bufs
        .get(tool_use_id)
        .map_or(&[] as &[u8], Vec::as_slice);
    if let Ok(args) = serde_json::from_slice::<Value>(buf) {
        registry.spawn(tool_use_id.to_owned(), meta, args);
        return true;
    }
    false
}

async fn drain_subscriber_into_response(
    mut sub: origin_stream::Subscriber,
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
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut registry = SpeculativeRegistry::default();
    let mut speculative_spawned: std::collections::HashSet<String> = std::collections::HashSet::new();

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
                    active_id = Some(id.to_owned());
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
                            && try_speculative_spawn(&id_owned, &tool_names, &tool_input_bufs, &mut registry)
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
    for (id, buf) in tool_input_bufs {
        let name = tool_names.get(&id).cloned().unwrap_or_default();
        blocks.push(Block::ToolUse {
            id,
            name,
            input_json: buf,
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
