// SPDX-License-Identifier: Apache-2.0
//! Task class taxonomy.

/// Coarse priority/budget bucket for every spawned task in the daemon.
///
/// Lower-numbered classes are more important. The runtime enforces a per-class
/// semaphore permit count; `Bulk` is additionally gated by a watcher that
/// parks it while any `Critical` permit is held.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TaskClass {
    /// Agent loop turns; provider HTTP/2; tool exec; swarm worker bodies.
    Critical = 0,
    /// Renderer ticks; IPC event dispatch; per-stream relays.
    Realtime = 1,
    /// Sidecar small-model jobs; MCP server clients; hook dispatch.
    Sidecar = 2,
    /// CAS GC; `SQLite` vacuum; memory idle consolidation.
    Background = 3,
    /// Initial code-graph build; bulk MCP discovery. Paused when `Critical`
    /// has any in-flight work.
    Bulk = 4,
}

impl TaskClass {
    pub const COUNT: usize = 5;

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Realtime => "realtime",
            Self::Sidecar => "sidecar",
            Self::Background => "background",
            Self::Bulk => "bulk",
        }
    }
}
