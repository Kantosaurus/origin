//! IPC request/response shapes for daemon ↔ client.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct PromptRequest {
    pub system: String,
    pub model: String,
    pub user_text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PromptReply {
    pub assistant_text: String,
    pub turns: u32,
}

/// Client→daemon control frames carried inside `Request` IPC frames.
///
/// P6.7 introduces a `kind`-tagged enum so the daemon can route per-frame
/// commands (memory accept/reject/edit) without a second socket. Legacy
/// clients that still send raw [`PromptRequest`] JSON are handled by a
/// fallback in the daemon main (`from_legacy_prompt_request`).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientMessage {
    Prompt(PromptRequest),
    MemoryDecision { proposal_id: u32, action: MemoryAction },
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
    /// Surfaced at turn end when the [`origin_mem::Proposer`] extracts a
    /// memory candidate from the user/assistant exchange. The CLI displays
    /// these and lets the user `/mem accept|reject|edit <id>` them.
    MemoryProposed {
        proposal_id: u32,
        body: String,
        suggested_tags: Vec<String>,
    },
}
