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
