//! `Session` — in-memory message log + metadata.
//!
//! Persistence (P1.12) wraps this with `SQLite` writes per turn.

use origin_core::types::{Message, MessageId};

#[derive(Debug)]
pub struct Session {
    pub id: MessageId,
    pub provider_name: String,
    pub model: String,
    pub messages: Vec<Message>,
}

impl Session {
    #[must_use]
    pub fn new(provider_name: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            provider_name: provider_name.into(),
            model: model.into(),
            messages: Vec::new(),
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
