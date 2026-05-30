// SPDX-License-Identifier: Apache-2.0
//! `Session` — in-memory message log + metadata.
//!
//! Persistence (P1.12) wraps this with `SQLite` writes per turn.

use origin_core::types::{Message, MessageId};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug)]
pub struct Session {
    pub id: String,
    pub provider_name: String,
    pub model: String,
    pub messages: Vec<Message>,
    /// Memory proposals emitted at the end of an assistant turn that the user
    /// has not yet accepted/rejected/edited. Keyed by `proposal_id`.
    pub pending_proposals: HashMap<u32, origin_mem::MemoryProposal>,
    /// Monotonic counter handed to [`origin_mem::Proposer::scan`].
    pub next_proposal_id: u32,
    /// Additional workspace roots the agent may read/edit across (cline
    /// multi-root workspaces). Empty ⇒ single-root behaviour, and the assembled
    /// system prompt is byte-identical. Populated from `PromptRequest.roots`.
    pub roots: Vec<PathBuf>,
}

impl Session {
    #[must_use]
    pub fn new(provider_name: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            id: MessageId::new().to_string(),
            provider_name: provider_name.into(),
            model: model.into(),
            messages: Vec::new(),
            pending_proposals: HashMap::new(),
            next_proposal_id: 1,
            roots: Vec::new(),
        }
    }

    /// Construct a session with a caller-supplied id. Used by admin/restore
    /// paths that need to materialize a known session id (e.g. when loading
    /// from `SessionStore` or in tests). Mirrors [`Session::new`] otherwise,
    /// leaving `provider_name` empty.
    #[must_use]
    pub fn new_with_id(id: String, model: String) -> Self {
        Self {
            id,
            provider_name: String::new(),
            model,
            messages: Vec::new(),
            pending_proposals: HashMap::new(),
            next_proposal_id: 1,
            roots: Vec::new(),
        }
    }

    pub fn push(&mut self, m: Message) {
        self.messages.push(m);
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<Message> {
        self.messages.clone()
    }
}
