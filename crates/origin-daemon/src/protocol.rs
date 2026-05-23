//! IPC request/response shapes for daemon ↔ client.

use std::path::PathBuf;

use origin_plan::OpEnvelope;
use origin_resume_token::ResumeToken;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct PromptRequest {
    pub system: String,
    pub model: String,
    pub user_text: String,
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
    /// P13.4.2: resume a previously persisted session. The daemon
    /// counts the persisted message rows for `session_id`, reads any
    /// checkpointed [`ResumeToken`] from the `resume/` directory, and
    /// replies with [`StreamEvent::SessionResumed`] describing the
    /// hydratable state. Returns [`StreamEvent::AdminError`] if the
    /// session does not exist.
    ResumeSession { session_id: String },
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
    ActivateSkill { name: String },
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
    TurnEnd,
    /// Emitted after a successful `ClientMessage::SwitchAccount` so the CLI
    /// can confirm the new provider/account is in effect for subsequent
    /// prompts.
    ProviderActive {
        provider: String,
        account_id: String,
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
    SkillError { message: String },
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
    /// P13.4.2: negative acknowledgement carrying a human-readable error
    /// message. Used as the failure side of the admin mutation handlers.
    AdminError {
        message: String,
    },
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
