// SPDX-License-Identifier: Apache-2.0
//! Sandbox profile enum + stable wire ordinals.
//!
//! Ordinals are part of the public ABI: they are carried on
//! `LifecycleEvent::PreTool`/`PostTool` and over the hook IPC envelope, so
//! reordering or renumbering is a breaking change.

use serde::{Deserialize, Serialize};

/// Stable `u8` ordinal for a [`SandboxProfile`]. Carried across IPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProfileOrdinal(pub u8);

/// Per-tool sandbox profile. Compiled into `ToolMeta` so dispatch is a single
/// `enum` discriminant — no string lookup, no allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProfile {
    /// No sandbox layer; child inherits the daemon's privileges.
    #[default]
    Inherit,
    /// Read-only filesystem access scoped to the workspace + standard libs.
    ReadFs,
    /// Read-only outside workspace; read+write inside the session cwd.
    WriteCwd,
    /// Shell-class: read+write cwd, exec stdlib binaries, no network.
    Shell,
    /// Read-only fs + outbound HTTPS (443) + DNS. No write, no listen.
    Network,
}

impl SandboxProfile {
    /// Stable wire ordinal for this profile.
    #[must_use]
    pub const fn ordinal(self) -> ProfileOrdinal {
        ProfileOrdinal(match self {
            Self::Inherit => 0,
            Self::ReadFs => 1,
            Self::WriteCwd => 2,
            Self::Shell => 3,
            Self::Network => 4,
        })
    }

    /// Inverse of [`SandboxProfile::ordinal`].
    ///
    /// Returns `None` for ordinals that don't correspond to a known variant —
    /// callers should treat unknown ordinals as a protocol violation.
    #[must_use]
    pub const fn from_ordinal(o: ProfileOrdinal) -> Option<Self> {
        Some(match o.0 {
            0 => Self::Inherit,
            1 => Self::ReadFs,
            2 => Self::WriteCwd,
            3 => Self::Shell,
            4 => Self::Network,
            _ => return None,
        })
    }
}
