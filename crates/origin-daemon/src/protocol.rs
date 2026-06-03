// SPDX-License-Identifier: Apache-2.0
//! IPC request/response shapes for daemon ↔ client.

use std::path::PathBuf;

use origin_plan::OpEnvelope;
use origin_resume_token::ResumeToken;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PromptRequest {
    pub system: String,
    pub model: String,
    pub user_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Optional reasoning-effort level for this turn, as a canonical wire token
    /// (`fast`/`low`/`medium`/`high`/`max`). `None` (the default) leaves the
    /// provider wire byte-identical to the pre-effort behavior. The daemon maps
    /// this to [`origin_provider::ReasoningEffort`] when building the
    /// `ChatRequest`. *Closes: claude-code `/effort`+`/fast`.*
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Optional extended-thinking budget (in tokens) for this turn. `None` (the
    /// default) leaves the provider wire byte-identical. The daemon threads this
    /// onto `LoopOptions.thinking_tokens`, which the Anthropic encoder maps to
    /// `"thinking": {"type":"enabled","budget_tokens": n}` (bumping `max_tokens`
    /// above `n`); other providers ignore it. *Closes: aider `--thinking-tokens`.*
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_tokens: Option<u32>,
    /// Multimodal attachments (images / extracted PDF text) to append to the
    /// FIRST user turn. Empty by default ⇒ text-only wire unchanged. The CLI
    /// encodes each file via `origin_multimodal::to_content_block` so the
    /// daemon never reads client-side paths itself. *Closes: aider images;
    /// gemini PDF/sketch; claude multimodal input.*
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<origin_multimodal::ContentBlock>,
    /// Read-only "plan mode": when `true`, the daemon downgrades every mutating
    /// tool to Deny for this turn so the model can only read/plan, never edit or
    /// run commands. `false` (the default) ⇒ unchanged. *Closes: gemini Plan
    /// Mode (policy-enforced read-only design phase).*
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
    /// Additional workspace roots the agent may operate across (cline multi-root
    /// workspaces). Empty (the default) ⇒ single-root behaviour, wire unchanged.
    /// Surfaced to the model via a `<workspace-roots>` system-prompt block.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<String>,
    /// Opt-in interactive permission prompting. When `true`, the daemon routes
    /// `RequiresPermission` tools (Bash/Write/Edit/…) through an IPC prompter
    /// that emits [`StreamEvent::PermissionAsk`] and waits for the client's
    /// [`ClientMessage::PermissionDecision`] before running the tool. `false`
    /// (the default) keeps the historical auto-allow behaviour, so the wire and
    /// tool execution are byte-identical. Headless/swarm never set this.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub permission_ask: bool,
}

/// Request to rebuild the code graph over a set of paths.
///
/// Wired into the daemon's IPC `Frame` dispatcher in P10; for P7.8 the shape
/// exists so the `post-commit` hook script and the agent free function in
/// `agent.rs` can share a single struct.
#[derive(Debug, Serialize, Deserialize)]
pub struct RebuildRequest {
    pub paths: Vec<PathBuf>,
}

/// Counter reply for a rebuild pass. Mirrors
/// [`origin_codegraph::rebuild::RebuildReport`] over the wire so the daemon
/// doesn't have to re-export the codegraph type.
#[derive(Debug, Serialize, Deserialize)]
pub struct RebuildReply {
    pub paths_seen: usize,
    pub nodes_added: usize,
    pub nodes_updated: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PromptReply {
    pub assistant_text: String,
    pub turns: u32,
}

/// Inbound IPC message from the CLI.
///
/// Internally tagged on `kind` so the daemon can dispatch `Prompt` requests
/// vs runtime control messages (e.g. `/account` switches, `/mem` memory
/// decisions) over the same `Request` frame.
///
/// P6.7 introduces `MemoryDecision`; P8.9 introduces `SwitchAccount`.
/// Legacy clients that still send raw [`PromptRequest`] JSON are handled by
/// a fallback in the daemon main (`from_legacy_prompt_request`).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientMessage {
    /// A user prompt to run through the agent loop.
    Prompt(PromptRequest),
    /// Client's answer to a [`StreamEvent::PermissionAsk`], correlated by `id`.
    /// `allow == false` denies the tool. Sent mid-turn over the same connection
    /// serving the prompt (like [`ClientMessage::Interrupt`]). Only produced
    /// when the turn opted into [`PromptRequest::permission_ask`].
    PermissionDecision { id: u64, allow: bool },
    /// Hot-swap the active provider/account credential without restarting
    /// the daemon.
    SwitchAccount { provider: String, account_id: String },
    /// User decision on a pending memory proposal surfaced via
    /// [`StreamEvent::MemoryProposed`].
    MemoryDecision { proposal_id: u32, action: MemoryAction },
    /// P13.2: ask the daemon to start a pairing session and emit a
    /// 6-digit code. The daemon replies with
    /// [`StreamEvent::PairCode`].
    PairStart { ttl_secs: u32 },
    /// P13.2: redeem a code previously surfaced by `PairStart`, binding
    /// it to `device_id`. On success the daemon replies with
    /// [`StreamEvent::PairIssued`]; failures use
    /// [`StreamEvent::PairError`].
    PairRedeem { code: String, device_id: String },
    /// P13.4.2: enumerate persisted sessions. The daemon replies with a
    /// single [`StreamEvent::SessionsListed`] carrying a row per session.
    ListSessions,
    /// P13.4.2: delete a session (and all of its message rows) by id. The
    /// daemon replies with [`StreamEvent::AdminOk`] on success or
    /// [`StreamEvent::AdminError`] on failure.
    RemoveSession { session_id: String },
    /// Conversation rewind: keep the first `keep_turns` message rows of
    /// `session_id` and delete the rest, rolling the transcript back to an
    /// earlier point (the session row is preserved so it can still be resumed).
    /// The daemon replies with [`StreamEvent::AdminOk`] on success or
    /// [`StreamEvent::AdminError`] on failure. *Closes: gemini `/rewind` chat
    /// revert (the transcript half; file revert is `origin rewind`).*
    RewindSession { session_id: String, keep_turns: u32 },
    /// P13.4.2: resume a previously persisted session. The daemon
    /// counts the persisted message rows for `session_id`, reads any
    /// checkpointed [`ResumeToken`] from the `resume/` directory, and
    /// replies with [`StreamEvent::SessionResumed`] describing the
    /// hydratable state. Returns [`StreamEvent::AdminError`] if the
    /// session does not exist.
    ResumeSession { session_id: String },
    /// Cross-harness *live resume*: hydrate a brand-new resumable origin
    /// session from a foreign harness's transcript. `source` is the originating
    /// harness tag (`claude-code` | `jcode` | `opencode`, plus the aliases
    /// [`origin_migrate::reconstruct::SourceKind::from_tag`] accepts); `path` is
    /// the external session file or harness root directory. The daemon
    /// reconstructs the transcript via [`origin_migrate::reconstruct`], creates a
    /// new session seeded with those messages, and replies with
    /// [`StreamEvent::ForeignResumed`] (or [`StreamEvent::AdminError`] on an
    /// unknown source / parse / I/O failure). *Closes: jcode L227 cross-harness
    /// session import AND resume.*
    ResumeForeign { source: String, path: String },
    /// P13.4.2: ask the daemon for a per-provider/per-model token usage
    /// snapshot. The daemon replies with [`StreamEvent::UsageReport`].
    GetUsage,
    /// P13.4.2: store (or overwrite) a provider secret in the keyvault.
    /// The daemon replies with [`StreamEvent::AdminOk`] or
    /// [`StreamEvent::AdminError`].
    KeyringAdd {
        provider: String,
        account: String,
        secret: String,
    },
    /// P13.4.2: list every account known to the keyvault for `provider`.
    /// The daemon replies with [`StreamEvent::KeyringAccounts`].
    KeyringList { provider: String },
    /// P13.4.2: delete a provider/account secret from the keyvault. The
    /// daemon replies with [`StreamEvent::AdminOk`] or
    /// [`StreamEvent::AdminError`].
    KeyringRemove { provider: String, account: String },
    /// Supervisor → daemon. Sent by `origin-supervisor` on restart for
    /// each previously-checkpointed open session. The daemon looks up
    /// the session, hydrates the transcript from CAS up to
    /// `token.last_turn`, and re-spawns any `pending_tool_calls` under
    /// `TaskClass::Critical`. The full hydrate-from-CAS wiring is a P14
    /// polish item; P12 ships the wire shape + an immediate ack handler.
    ResumeRequest { token: ResumeToken },
    /// Push `name` onto this connection's active skill stack. The daemon
    /// looks up the skill in its `SkillCatalog`; on success it replies with
    /// [`StreamEvent::SkillActive`] carrying the skill's `allowed-tools`
    /// (so the CLI can render the narrowing it just applied). On failure
    /// (skill not in catalog) it replies with [`StreamEvent::SkillError`].
    ActivateSkill { name: String, args: Option<String> },
    /// Pop the named skill off this connection's active stack (the
    /// rightmost match if the same skill was activated multiple times).
    /// Always replies with [`StreamEvent::AdminOk`] — deactivating an
    /// inactive skill is not an error.
    DeactivateSkill { name: String },
    /// Walk `name`'s steps in `~/.origin/workflows.toml`, activating the
    /// FIRST resolvable step's skill on this connection's stack. The
    /// daemon replies with [`StreamEvent::WorkflowStepActive`] for the
    /// active step, or [`StreamEvent::WorkflowActive`] (with empty
    /// `steps`) when no step resolves, or [`StreamEvent::SkillError`]
    /// when the workflow name isn't found. Subsequent steps activate
    /// one-at-a-time after each successful `Prompt`.
    ActivateWorkflow { name: String },
    /// Subscribe this connection to the daemon-wide plan-op broadcast.
    /// Every subsequent [`OpEnvelope`] published to the bus is forwarded as
    /// a [`StreamEvent::PlanOp`] event frame. The subscription terminates
    /// when the connection closes.
    SubscribePlan,
    /// Export a persisted session transcript. `format` is `"md"` (Markdown)
    /// or `"json"`. The daemon loads the message log, renders it via
    /// `origin_export`, and replies with [`StreamEvent::SessionExport`], or
    /// [`StreamEvent::AdminError`] if the session does not exist.
    ExportSession { session_id: String, format: String },
    /// User-issued cancel. Clears any in-flight goal iteration. The outer
    /// message loop continues running afterward — the connection stays open.
    ///
    /// When sent mid-`drive_goal_loop` the driver's peek catches it between
    /// iterations, emits `GoalCleared { UserSlash }`, and returns control
    /// to the outer message loop without consuming another provider call.
    /// When sent with no goal active the outer loop emits no event — the
    /// signal is harmless.
    Interrupt,
    /// `/clear`: mechanically reset the in-session context. This is a
    /// first-class admin verb, NOT a skill activation — it never touches the
    /// per-connection skill stack or the skill catalog.
    ///
    /// The daemon terminates any active goal (emitting
    /// [`StreamEvent::GoalCleared`] with [`origin_goal::ClearReasonWire::UserClearAll`]
    /// and writing the terminal-status checkpoint so a crash cannot resurrect
    /// the discarded goal), then replies with [`StreamEvent::AdminOk`]. With no
    /// active goal it is a single `AdminOk`.
    ClearAll,
}

impl ClientMessage {
    /// Convenience constructor for the common `Prompt` variant.
    #[must_use]
    pub const fn prompt(req: PromptRequest) -> Self {
        Self::Prompt(req)
    }
}

/// Action the user took on a pending memory proposal.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MemoryAction {
    Accept,
    Reject,
    Edit { body: String, tags: Vec<String> },
}

/// One in-flight event during a streaming response. Encoded as JSON inside
/// an IPC `Event` frame body so the CLI can decode without depending on rkyv.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamEvent {
    TextDelta {
        text: String,
    },
    ToolUseDelta {
        partial_json: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    Usage {
        input_tokens: u32,
        output_tokens: u32,
        cache_read_input_tokens: u32,
        cache_creation_input_tokens: u32,
    },
    ToolActivity {
        tool: String,
        summary: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        diff_lines: Vec<DiffLine>,
    },
    /// One incremental chunk of streaming output from a tool (today: only
    /// `Bash`). Emitted line-by-line while the tool is still running so
    /// the user sees output appear live rather than after completion.
    /// `ToolResult` is normally suppressed for `Bash` because the chunks
    /// already convey the output — except when zero chunks were emitted
    /// (silent commands), in which case `run_bash_streaming` falls back
    /// to a short `ToolResult` so completion is still visible.
    ToolChunk {
        tool: String,
        content: String,
    },
    /// Emitted by the agent loop AFTER a tool dispatch completes (success
    /// or failure). Carries a truncated preview of the tool's output so the
    /// CLI can show *what the tool actually did* — without this the user
    /// sees only the `ToolActivity` start line and a silent gap while the
    /// LLM consumes the result.
    ToolResult {
        tool: String,
        ok: bool,
        /// Truncated UTF-8-lossy preview of the result bytes. Bounded in
        /// the daemon so the wire frame stays small; the LLM still sees
        /// the full body via the `Block::ToolResult` round-trip.
        preview: String,
        /// Number of bytes elided from the preview. `0` when the full
        /// result fit. The CLI uses this to render a "+N bytes omitted"
        /// affordance.
        #[serde(default)]
        elided_bytes: u32,
    },
    TurnEnd,
    /// Opt-in interactive permission ask (gated by
    /// [`PromptRequest::permission_ask`]). Emitted before a `RequiresPermission`
    /// tool runs; the daemon then blocks on the matching
    /// [`ClientMessage::PermissionDecision`] (correlated by `id`). Never emitted
    /// in the default (auto-allow) path, so default streams are byte-identical.
    PermissionAsk {
        /// Correlation id, unique within a turn. Echoed back in the decision.
        id: u64,
        /// Tool name (e.g. `Bash`, `Write`).
        tool: String,
        /// Truncated, human-readable preview of the tool arguments (the command
        /// or path) so the user sees *what* they are approving.
        args_preview: String,
    },
    /// Emitted after a successful `ClientMessage::SwitchAccount` so the CLI
    /// can confirm the new provider/account is in effect for subsequent
    /// prompts.
    ProviderActive {
        provider: String,
        account_id: String,
    },
    /// Emitted by the agent loop just before it sleeps inside a rate-limit
    /// retry backoff. Without this the CLI cannot distinguish a 60-second
    /// `tokio::time::sleep` from a hang — both look like the same silence.
    /// `attempt` is 1-indexed (first retry == 1); `max_attempts` is the
    /// total budget (`MAX_PROVIDER_RETRIES + 1`, i.e. the initial call
    /// plus the retry cap).
    ProviderBackoff {
        retry_in_secs: u32,
        attempt: u32,
        max_attempts: u32,
    },
    /// Surfaced at turn end when the [`origin_mem::Proposer`] extracts a
    /// memory candidate from the user/assistant exchange. The CLI displays
    /// these and lets the user `/mem accept|reject|edit <id>` them.
    MemoryProposed {
        proposal_id: u32,
        body: String,
        suggested_tags: Vec<String>,
    },
    /// P13.2: response to a [`ClientMessage::PairStart`]. Surfaces the
    /// 6-digit code the daemon operator must read aloud / paste to the
    /// remote client.
    PairCode {
        code: String,
        expires_in_secs: u32,
    },
    /// P13.2: response to a successful
    /// [`ClientMessage::PairRedeem`]. Carries the freshly minted bearer
    /// token and the bound device id.
    PairIssued {
        bearer: String,
        device_id: String,
        ttl_secs: u32,
    },
    /// P13.2: response to a failed [`ClientMessage::PairRedeem`]. The
    /// `message` is the human-readable rendering of `PairingError`.
    PairError {
        message: String,
    },
    /// P13.4.2: response to [`ClientMessage::ListSessions`]. Carries one
    /// wire-shape summary per persisted session, newest-first.
    SessionsListed {
        summaries: Vec<SessionSummaryWire>,
    },
    /// P13.4.2: response to [`ClientMessage::GetUsage`]. Carries one row
    /// per (provider, model) tuple seen in the running metrics registry.
    UsageReport {
        rows: Vec<UsageRow>,
    },
    /// P13.4.2: response to [`ClientMessage::KeyringList`]. Carries the
    /// list of accounts the keyvault knows for `provider`.
    KeyringAccounts {
        provider: String,
        accounts: Vec<String>,
    },
    /// Response to [`ClientMessage::ExportSession`]: the rendered transcript
    /// (Markdown or JSON depending on the requested format).
    SessionExport { content: String },
    /// P13.4.2: positive acknowledgement for admin mutations that have no
    /// payload of their own (`RemoveSession`, `KeyringAdd`, …).
    AdminOk,
    /// One plan op envelope forwarded to a [`ClientMessage::SubscribePlan`]
    /// subscriber. The CLI feeds these into its `plan_panel_wiring::ingest`
    /// call site to drive the side-panel render.
    PlanOp {
        envelope: OpEnvelope,
    },
    /// Response to [`ClientMessage::ResumeSession`]. `messages_loaded` is
    /// the count of persisted message rows that would be re-hydrated;
    /// `restored_to_turn` is the `last_turn` of the checkpointed
    /// [`ResumeToken`] when one exists, otherwise `messages_loaded - 1`
    /// (or `0` for empty sessions). `had_resume_token` flags whether the
    /// supervisor previously checkpointed the session.
    SessionResumed {
        session_id: String,
        messages_loaded: u32,
        restored_to_turn: u32,
        had_resume_token: bool,
    },
    /// Response to [`ClientMessage::ResumeForeign`]. `session_id` is the
    /// freshly-created origin session seeded with the reconstructed foreign
    /// transcript; `messages_loaded` is the number of message rows persisted;
    /// `suggested_model` is the origin-catalog model the new session adopted
    /// (mapped from the foreign session's model family via
    /// [`origin_migrate::reconstruct::suggest_model`]). The new session is a
    /// first-class resumable origin session (`origin sessions resume
    /// <session_id>`).
    ForeignResumed {
        session_id: String,
        messages_loaded: u32,
        suggested_model: String,
    },
    /// Positive ack for a successful [`ClientMessage::ActivateSkill`].
    /// `allowed_tools` is the intersection mask currently in effect after
    /// pushing this skill — the CLI displays it so users can see what
    /// they've just narrowed access to.
    SkillActive {
        name: String,
        allowed_tools: Vec<String>,
    },
    /// Negative ack for [`ClientMessage::ActivateSkill`] — typically the
    /// requested skill is not in the daemon's catalog.
    SkillError {
        message: String,
    },
    /// Ack for a successful [`ClientMessage::ActivateWorkflow`]. `steps` is
    /// the ordered list of skill names that were activated; `skipped` lists
    /// any steps whose skills weren't found in the catalog. Both are surfaced
    /// in a single frame so the CLI renders the full outcome with one read.
    WorkflowActive {
        name: String,
        steps: Vec<String>,
        #[serde(default)]
        skipped: Vec<String>,
    },
    /// Emitted both on initial [`ClientMessage::ActivateWorkflow`] (for
    /// the first resolvable step) and after each successful `Prompt`
    /// while a workflow is in progress (for the next resolvable step).
    /// Step activation is gated on prompt completion — only one step's
    /// skill is on the stack at a time.
    ///
    /// `step_index` is the 0-based index into the workflow's `steps` of
    /// the step now in effect. `total_steps` is the length of that
    /// vector. `skill` is the catalog name of the active skill.
    /// `skipped` lists any earlier steps walked past during this
    /// transition because they had no catalog match.
    WorkflowStepActive {
        name: String,
        step_index: u32,
        total_steps: u32,
        skill: String,
        #[serde(default)]
        skipped: Vec<String>,
    },
    /// Emitted after the last step's `Prompt` completes. The previous
    /// step's skill has already been deactivated by the daemon when this
    /// fires. `skipped` lists any trailing unresolvable steps walked
    /// past on the way to completion.
    WorkflowComplete {
        name: String,
        #[serde(default)]
        skipped: Vec<String>,
    },
    /// Emitted when a `Prompt` fails while a workflow is in progress.
    /// The workflow stays paused at the SAME step — its skill remains
    /// active on the connection's stack — and the next successful
    /// prompt advances. `message` is the provider/loop error
    /// rendering. Use this to render a "step held; retry to resume"
    /// indicator in the CLI.
    WorkflowStepHeld {
        name: String,
        step_index: u32,
        total_steps: u32,
        skill: String,
        message: String,
    },
    /// P13.4.2: negative acknowledgement carrying a human-readable error
    /// message. Used as the failure side of the admin mutation handlers.
    AdminError {
        message: String,
    },
    /// Emitted when `/goal <cond>` activates a new goal.
    GoalActive {
        condition: String,
        max_iter: u32,
        token_budget: u64,
    },
    /// Emitted when the user runs bare `/goal` and no goal is active on
    /// the connection. Distinct from [`StreamEvent::SkillError`] so the
    /// CLI renders it as a benign info line ("no active goal") rather
    /// than an error row.
    GoalInactive,
    /// Emitted after each `run_loop` tick while a goal is active.
    GoalIteration {
        iter: u32,
        tokens_spent: u64,
        last_tag: origin_goal::TagOutcomeWire,
    },
    /// Emitted right before the Haiku verifier call.
    GoalVerifying,
    /// Terminal event for a goal.
    GoalCleared {
        reason: origin_goal::ClearReasonWire,
        iter: u32,
        tokens_spent: u64,
    },
}

/// A single line in a unified diff view.
#[derive(Debug, Serialize, Deserialize)]
pub struct DiffLine {
    /// `"+"` for added, `"-"` for removed, `" "` for context.
    pub kind: String,
    /// 1-based line number (in old file for `-`, new file for `+`, either for context).
    pub line_no: u32,
    /// The text content of the line.
    pub text: String,
}

/// Wire-shape projection of `SessionStore::SessionSummary`.
///
/// Kept as a distinct type so the daemon can change its in-process row
/// shape (extra derived columns, debug fields, …) without breaking IPC
/// compatibility.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionSummaryWire {
    pub id: String,
    pub created_at: i64,
    pub title: Option<String>,
    pub model: String,
    pub message_count: u32,
}

/// One row of the `StreamEvent::UsageReport`. Derived from the metrics
/// registry's `origin_tokens_in_total{provider,model}` /
/// `origin_tokens_out_total{provider,model}` counter families.
#[derive(Debug, Serialize, Deserialize)]
pub struct UsageRow {
    pub provider: String,
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Estimated USD cost for this (provider, model) row under the centralized
    /// [`origin_cost`] pricing table. `0.0` when the model is unpriced.
    #[serde(default)]
    pub cost_usd: f64,
}

/// Outbound responses the daemon sends back to a client (or the supervisor).
///
/// Today the only variant is `ResumeAck`, sent in response to a
/// [`ClientMessage::ResumeRequest`]. We keep the enum tagged so future
/// non-event responses can be added without rev-locking the wire format.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Acknowledgement that the daemon accepted the resume token and
    /// hydrated the session up to `restored_to_turn`. P12 ships the wire
    /// shape; full hydrate-from-CAS plumbing is a P14 polish item.
    ResumeAck {
        session_id: String,
        restored_to_turn: u32,
    },
}

#[cfg(test)]
#[allow(clippy::panic)]
mod permission_wire_tests {
    use super::*;

    #[test]
    fn permission_ask_event_round_trips() {
        let ev = StreamEvent::PermissionAsk {
            id: 7,
            tool: "Bash".to_string(),
            args_preview: "rm -rf build/".to_string(),
        };
        let json = serde_json::to_string(&ev).expect("serialize");
        assert!(json.contains("\"kind\":\"permission_ask\""), "tagged on kind: {json}");
        let back: StreamEvent = serde_json::from_str(&json).expect("deserialize");
        match back {
            StreamEvent::PermissionAsk { id, tool, args_preview } => {
                assert_eq!(id, 7);
                assert_eq!(tool, "Bash");
                assert_eq!(args_preview, "rm -rf build/");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn permission_decision_message_round_trips() {
        let msg = ClientMessage::PermissionDecision { id: 7, allow: true };
        let json = serde_json::to_string(&msg).expect("serialize");
        let back: ClientMessage = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(back, ClientMessage::PermissionDecision { id: 7, allow: true }));
    }

    #[test]
    fn permission_ask_defaults_off_and_is_omitted_when_false() {
        // Byte-identical default: a request that never opts in must not emit the
        // `permission_ask` key at all (skip_serializing_if).
        let req = PromptRequest {
            user_text: "hi".to_string(),
            ..Default::default()
        };
        assert!(!req.permission_ask);
        let json = serde_json::to_string(&req).expect("serialize");
        assert!(!json.contains("permission_ask"), "default request omits the flag: {json}");
    }
}
