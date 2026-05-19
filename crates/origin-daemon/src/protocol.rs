//! IPC request/response shapes for daemon ↔ client.

use std::path::PathBuf;

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

/// Inbound IPC message from the CLI. Internally tagged on `kind` so the
/// daemon can dispatch `Prompt` requests vs runtime control messages
/// (e.g. `/account` switches) over the same `Request` frame.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientMessage {
    /// A user prompt to run through the agent loop. Fields mirror
    /// [`PromptRequest`] flattened into the message so the internally-tagged
    /// representation stays a single JSON map.
    Prompt {
        system: String,
        model: String,
        user_text: String,
    },
    /// Hot-swap the active provider/account credential without restarting
    /// the daemon.
    SwitchAccount { provider: String, account_id: String },
}

impl ClientMessage {
    /// Convenience constructor for the common `Prompt` variant.
    #[must_use]
    pub fn prompt(req: PromptRequest) -> Self {
        Self::Prompt {
            system: req.system,
            model: req.model,
            user_text: req.user_text,
        }
    }
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
}
