//! Snapshot compaction primitives (P9.3, N7.7).
//!
//! A [`Snapshot`] is the persistence-layer fast-forward record for a swarm
//! plan: rather than re-fold an unbounded op log on every restart, the
//! coordinator periodically writes the materialised plan state into the CAS
//! and records the resulting handle here. After the row lands,
//! [`crate::store::PlanStore::write_snapshot`] GCs every op below
//! `fully_acked_below` — those ops are now subsumed by the body.
//!
//! `Snapshot` ops can also flow over the wire as a member of the [`crate::ops::Op`]
//! alphabet (variant `Op::Snapshot`). When such an op is folded the action is
//! a no-op (snapshots do not mutate fold state — they short-circuit the load
//! path entirely). `serde` is implemented so the variant can be bincoded by
//! [`crate::store::PlanStore::append_op`] like any other op.

/// Snapshot record persisted in the V4 `plan_snapshots` table.
///
/// - `seq` — the Lamport timestamp of the snapshot op. Doubles as the
///   primary key, so writes are monotonic by construction.
/// - `state_handle` — 32-byte blake3 hash of the CAS-stored plan body, the
///   output of [`crate::plan::Plan::serialize_for_snapshot`].
/// - `fully_acked_below` — the lamport below which all workers have
///   acknowledged receipt. Ops with `lamport < fully_acked_below` are
///   subsumed by the snapshot and become GC-eligible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    /// Lamport of the snapshot op itself.
    pub seq: u64,
    /// CAS hash of the serialized [`crate::plan::Plan`] body.
    pub state_handle: [u8; 32],
    /// All ops with `lamport < fully_acked_below` are GC-eligible.
    pub fully_acked_below: u64,
}

impl Snapshot {
    /// Construct a new snapshot record.
    #[must_use]
    pub const fn new(seq: u64, state_handle: [u8; 32], fully_acked_below: u64) -> Self {
        Self {
            seq,
            state_handle,
            fully_acked_below,
        }
    }
}
