//! `TokenEvent` — rkyv-archived discriminated record.
//!
//! Per spec N4.4 the ring stores rkyv-archived `TokenEvent` records so the
//! provider stream parser, the renderer, and the tool-use parser can all read
//! the same bytes with no intermediate `String`.

use rkyv::{Archive, Deserialize, Serialize};

#[derive(Archive, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[archive(check_bytes)]
#[repr(u8)]
pub enum TokenKind {
    /// Streaming text delta from the assistant.
    TextDelta = 0,
    /// `tool_use` JSON delta (full input arrives in fragments).
    ToolUseDelta = 1,
    /// `thinking` token delta (extended thinking).
    ThinkingDelta = 2,
    /// Provider boundary: turn complete, no more deltas this round.
    TurnEnd = 3,
    /// Provider sent usage stats after `message_stop`.
    Usage = 4,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[archive(check_bytes)]
pub struct TokenEvent {
    kind: TokenKind,
    payload: Vec<u8>,
}

impl TokenEvent {
    #[must_use]
    pub const fn new(kind: TokenKind, payload: Vec<u8>) -> Self {
        Self { kind, payload }
    }

    #[must_use]
    pub const fn kind(&self) -> TokenKind {
        self.kind
    }

    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}
