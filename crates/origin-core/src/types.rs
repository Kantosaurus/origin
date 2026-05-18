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
