//! IPC request/response shapes for daemon ↔ client.

use std::path::PathBuf;

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
    /// Supervisor → daemon. Sent by `origin-supervisor` on restart for
    /// each previously-checkpointed open session. The daemon looks up
    /// the session, hydrates the transcript from CAS up to
    /// `token.last_turn`, and re-spawns any `pending_tool_calls` under
    /// `TaskClass::Critical`. The full hydrate-from-CAS wiring is a P14
    /// polish item; P12 ships the wire shape + an immediate ack handler.
    ResumeRequest { token: ResumeToken },
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
