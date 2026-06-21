// SPDX-License-Identifier: Apache-2.0
use rkyv::{Archive, Deserialize, Serialize};

#[derive(Archive, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[archive(check_bytes)]
#[repr(u8)]
pub enum Role {
    User,
    Assistant,
    Tool,
    System,
}

impl Role {
    #[must_use]
    pub const fn from_archived(a: &ArchivedRole) -> Self {
        match a {
            ArchivedRole::User => Self::User,
            ArchivedRole::Assistant => Self::Assistant,
            ArchivedRole::Tool => Self::Tool,
            ArchivedRole::System => Self::System,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageId(pub ulid::Ulid);

impl MessageId {
    #[must_use]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(ulid::Ulid::new())
    }
}

impl core::fmt::Display for MessageId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TurnIndex(pub u32);

impl TurnIndex {
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(n) => Some(Self(n)),
            None => None,
        }
    }
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[archive(check_bytes)]
#[repr(u8)]
pub enum CacheBoundary {
    Frozen,
    Sticky,
    Sliding,
}

// The largest variant (ToolResult) carries at most a 32-byte inline hash array
// plus a small Vec<u8> — all stack-allocated fields are small. The size
// difference between variants is intentional and acceptable for this domain.
#[allow(clippy::large_enum_variant)]
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[archive(check_bytes)]
pub enum Block {
    Text {
        text: String,
        cache_marker: Option<CacheBoundary>,
    },
    ToolUse {
        id: String,
        name: String,
        input_json: Vec<u8>,
        cache_marker: Option<CacheBoundary>,
    },
    ToolResult {
        tool_use_id: String,
        handle: Option<[u8; 32]>,
        inline: Option<Vec<u8>>,
        cache_marker: Option<CacheBoundary>,
    },
    Thinking {
        tokens: String,
        signature: Option<String>,
    },
}

impl Block {
    // Not const: `s.into()` (Into<String>) is not stable as const.
    #[must_use]
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text {
            text: s.into(),
            cache_marker: None,
        }
    }
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[archive(check_bytes)]
pub struct Message {
    pub role: Role,
    pub blocks: Vec<Block>,
}

impl Message {
    #[must_use]
    pub const fn new(role: Role) -> Self {
        Self {
            role,
            blocks: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_block(mut self, b: Block) -> Self {
        self.blocks.push(b);
        self
    }
}

/// Defensively enforce the Anthropic Messages API tool-pairing invariant.
///
/// Removes every `ToolResult` whose `tool_use_id` has no matching `ToolUse` in
/// the immediately-preceding (kept) message, and drops any message left empty
/// as a result.
///
/// The provider rejects an orphaned `tool_result` with a hard
/// `400 ... unexpected tool_use_id found in tool_result blocks`. That can happen
/// when a transcript is corrupted by an upstream bug — a reused session id whose
/// stale tail got spliced on, a compaction hole, a hand-edited store, a
/// migration — and a single malformed turn deep in the history takes down the
/// whole request. Running a transcript through this pass right before it is
/// loaded or sent makes such corruption self-healing instead of fatal.
///
/// A well-formed transcript is returned byte-identical. "Previous message" is
/// evaluated against the message that precedes each entry in the *repaired*
/// output, so dropping an orphaned tool turn never knocks a valid pair out of
/// alignment.
#[must_use]
pub fn strip_orphan_tool_results(messages: Vec<Message>) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    for mut msg in messages {
        // `tool_use` ids advertised by the last message we actually kept.
        let prev_ids: std::collections::HashSet<String> = out
            .last()
            .map(|p| {
                p.blocks
                    .iter()
                    .filter_map(|b| match b {
                        Block::ToolUse { id, .. } => Some(id.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let has_orphan = msg.blocks.iter().any(
            |b| matches!(b, Block::ToolResult { tool_use_id, .. } if !prev_ids.contains(tool_use_id.as_str())),
        );
        if has_orphan {
            msg.blocks.retain(|b| match b {
                Block::ToolResult { tool_use_id, .. } => prev_ids.contains(tool_use_id.as_str()),
                _ => true,
            });
            // A tool turn whose every result was orphaned is now empty — drop it.
            if msg.blocks.is_empty() {
                continue;
            }
        }
        out.push(msg);
    }
    out
}

/// Drop a trailing `Assistant` message that ends the transcript with a `ToolUse`
/// carrying no following `tool_result`.
///
/// Anthropic rejects a request whose FINAL message is an assistant `tool_use`
/// with no answer (`400 ... tool_use ... ids were found without tool_result`).
/// The daemon appends results before its next call, so this only arises from an
/// interrupt, a resumed/corrupted transcript, or a migration — running it at the
/// wire choke makes those self-healing. A well-formed transcript (last message a
/// user prompt or tool result) returns unchanged.
#[must_use]
pub fn strip_trailing_orphan_tool_use(mut messages: Vec<Message>) -> Vec<Message> {
    while messages.last().is_some_and(|m| {
        matches!(m.role, Role::Assistant) && m.blocks.iter().any(|b| matches!(b, Block::ToolUse { .. }))
    }) {
        messages.pop();
    }
    messages
}

#[cfg(test)]
mod tool_pairing_tests {
    use super::{strip_orphan_tool_results, strip_trailing_orphan_tool_use, Block, Message, Role};

    fn assistant_tool_use(id: &str) -> Message {
        Message {
            role: Role::Assistant,
            blocks: vec![
                Block::text("call"),
                Block::ToolUse {
                    id: id.into(),
                    name: "Read".into(),
                    input_json: b"{}".to_vec(),
                    cache_marker: None,
                },
            ],
        }
    }
    fn tool_result(id: &str) -> Message {
        Message {
            role: Role::Tool,
            blocks: vec![Block::ToolResult {
                tool_use_id: id.into(),
                handle: None,
                inline: Some(b"r".to_vec()),
                cache_marker: None,
            }],
        }
    }

    #[test]
    fn well_formed_transcript_is_unchanged() {
        let t = vec![
            Message::new(Role::User).with_block(Block::text("hi")),
            assistant_tool_use("a"),
            tool_result("a"),
            Message::new(Role::Assistant).with_block(Block::text("done")),
        ];
        assert_eq!(strip_orphan_tool_results(t.clone()), t);
    }

    #[test]
    fn trailing_orphan_tool_use_is_stripped() {
        // A transcript ending in an assistant tool_use with no following result
        // (interrupt / resumed-corrupt) would 400 — strip the trailing turn.
        let t = vec![
            Message::new(Role::User).with_block(Block::text("hi")),
            assistant_tool_use("a"),
        ];
        let out = strip_trailing_orphan_tool_use(t);
        assert_eq!(out.len(), 1, "the orphan trailing tool_use turn is dropped");
        assert!(matches!(out[0].role, Role::User));
        // A well-formed transcript (ends in a tool result) is untouched.
        let ok = vec![
            Message::new(Role::User).with_block(Block::text("hi")),
            assistant_tool_use("a"),
            tool_result("a"),
        ];
        assert_eq!(strip_trailing_orphan_tool_use(ok.clone()), ok);
    }

    #[test]
    fn orphan_after_terminal_text_turn_is_dropped() {
        // The exact production seam: a terminal text summary turn followed by a
        // stranded tool_result with no matching tool_use.
        let t = vec![
            Message::new(Role::User).with_block(Block::text("hi")),
            Message::new(Role::Assistant).with_block(Block::text("done, here's a summary")),
            tool_result("ghost"),
        ];
        let out = strip_orphan_tool_results(t);
        assert_eq!(out.len(), 2);
        assert!(out
            .iter()
            .all(|m| !m.blocks.iter().any(|b| matches!(b, Block::ToolResult { .. }))));
    }

    #[test]
    fn matched_result_kept_orphan_in_same_message_dropped() {
        let t = vec![
            assistant_tool_use("a"),
            Message {
                role: Role::Tool,
                blocks: vec![
                    Block::ToolResult {
                        tool_use_id: "a".into(),
                        handle: None,
                        inline: Some(b"ok".to_vec()),
                        cache_marker: None,
                    },
                    Block::ToolResult {
                        tool_use_id: "b".into(),
                        handle: None,
                        inline: Some(b"stale".to_vec()),
                        cache_marker: None,
                    },
                ],
            },
        ];
        let out = strip_orphan_tool_results(t);
        assert_eq!(out.len(), 2);
        let ids: Vec<&str> = out[1]
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["a"]);
    }
}
