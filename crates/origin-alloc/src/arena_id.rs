//! Stable enumeration of per-component allocator arenas.

/// Identifies a logical allocator arena. The backend (jemalloc or no-op) is
/// chosen by cargo feature; the same `ArenaId` resolves to the same arena
/// inside a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ArenaId {
    /// Agent-loop turn buffers, message log staging, cache-planner scratch.
    Agent = 0,
    /// CAS write buffers and decompression scratch.
    Cas = 1,
    /// Sidecar small-model worker — summaries, structure extraction.
    Sidecar = 2,
    /// Swarm coordinator state — plan ops, completion-report assembly.
    SwarmCoord = 3,
    /// Per-worker swarm allocations — `destroy`'d on worker exit.
    SwarmWorker = 4,
    /// IPC frame buffers and rkyv staging.
    Ipc = 5,
    /// `/metrics` Prometheus encoder scratch.
    MetricsHttp = 6,
    /// Code knowledge graph node/edge build buffers.
    CodeGraph = 7,
    /// Conversation memory graph and HNSW scratch.
    Mem = 8,
    /// Catch-all for short-lived allocations not classified above.
    Other = 9,
}

impl ArenaId {
    /// Number of variants. Hard-coded — keep in sync with the enum.
    pub const COUNT: usize = 10;

    /// 0-based dense index into the backend's per-arena tables.
    #[must_use]
    pub const fn backend_index(self) -> usize {
        self as usize
    }

    /// Stable, human-readable label for logs and metrics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Cas => "cas",
            Self::Sidecar => "sidecar",
            Self::SwarmCoord => "swarm_coord",
            Self::SwarmWorker => "swarm_worker",
            Self::Ipc => "ipc",
            Self::MetricsHttp => "metrics_http",
            Self::CodeGraph => "code_graph",
            Self::Mem => "mem",
            Self::Other => "other",
        }
    }
}
