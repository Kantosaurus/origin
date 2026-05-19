//! The four prefix bands the `CachePlanner` sorts sections into.

/// Ordering used when building the request.
///
/// Cache markers are placed at every adjacent-band boundary. Volatile content
/// is always last because it is the most likely to change between turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Band {
    /// System prompt + tool schemas. Stable across all sessions.
    Frozen = 0,
    /// Long-lived skill injections, project context, recalled memories.
    Sticky = 1,
    /// Stable recent conversation prefix (older than the active turn).
    Sliding = 2,
    /// This turn's new injections / fresh tool results.
    Volatile = 3,
}

impl Band {
    /// Promotion target one band closer to Frozen, or `None` if already Frozen.
    #[must_use]
    pub const fn promoted(self) -> Option<Self> {
        match self {
            Self::Frozen => None,
            Self::Sticky => Some(Self::Frozen),
            Self::Sliding => Some(Self::Sticky),
            Self::Volatile => Some(Self::Sliding),
        }
    }

    /// Demotion target one band closer to Volatile, or `None` if already Volatile.
    #[must_use]
    pub const fn demoted(self) -> Option<Self> {
        match self {
            Self::Frozen => Some(Self::Sticky),
            Self::Sticky => Some(Self::Sliding),
            Self::Sliding => Some(Self::Volatile),
            Self::Volatile => None,
        }
    }
}
