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
