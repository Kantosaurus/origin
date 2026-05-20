//! Op-log alphabet for the shared plan CRDT (P9.1).
//!
//! Every op is wrapped in an [`OpEnvelope`] carrying the producing
//! [`ActorId`] and a [`Lamport`] timestamp. The pair `(lamport, actor)` is the
//! canonical total order under which [`crate::fold::fold`] folds the log into
//! a deterministic [`crate::plan::Plan`].

use crate::lamport::{ActorId, Lamport, OpKey};
use crate::logoot::LogootKey;
use crate::snapshot::Snapshot;

/// Stable identifier for a step in the plan tree.
///
/// Carries a 128-bit value so it can hold a Ulid, a content-hash prefix, or
/// any other globally-unique payload chosen by the producer. The CRDT only
/// requires that distinct logical steps choose distinct ids — collisions are a
/// caller bug, not a fold-time failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StepId(u128);

impl StepId {
    /// Construct a `StepId` from a raw 128-bit value.
    #[must_use]
    pub const fn from_u128(value: u128) -> Self {
        Self(value)
    }

    /// Underlying value.
    #[must_use]
    pub const fn value(self) -> u128 {
        self.0
    }
}

// Serialize `StepId` as a 32-char lowercase hex string. `serde_json` does
// not support `u128` deserialization without the `arbitrary_precision`
// feature, which we deliberately don't pull in. Strings round-trip cleanly
// over every backend the daemon uses (IPC `serde_json` frames + persisted
// log files).
impl serde::Serialize for StepId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let hex = format!("{:032x}", self.0);
        s.serialize_str(&hex)
    }
}

impl<'de> serde::Deserialize<'de> for StepId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'_, str>>::deserialize(d)?;
        u128::from_str_radix(&s, 16)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

/// Lifecycle status of a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Status {
    /// Step exists but no worker has picked it up.
    Pending,
    /// A worker is currently leased to drive this step.
    InProgress,
    /// Step is complete and accepted.
    Done,
    /// Step was abandoned or rejected; kept for audit.
    Cancelled,
}

/// `AddStep(parent, id, body, key)` — insert a step into the plan tree at
/// position `key` under `parent` (root if `None`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AddStep {
    /// Globally-unique id chosen by the producer.
    pub id: StepId,
    /// Optional parent step (root list if `None`).
    pub parent: Option<StepId>,
    /// Initial body for the step.
    pub body: String,
    /// Logoot position key — see `Reorder` for later moves.
    pub key: LogootKey,
}

/// `MarkStep(id, status)` — set a step's status. Last-writer-wins on
/// `(lamport, actor)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MarkStep {
    /// Step being marked.
    pub id: StepId,
    /// New status.
    pub status: Status,
}

/// `EditContent(id, body)` — last-writer-wins replace of a step body.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EditContent {
    /// Step being edited.
    pub id: StepId,
    /// New body.
    pub body: String,
}

/// `AddNote(id, body)` — append a note to a step. Notes form an ordered list
/// driven by `(lamport, actor)`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AddNote {
    /// Step the note is attached to.
    pub id: StepId,
    /// Note body.
    pub body: String,
}

/// `Reorder(id, key)` — move a step to a new Logoot position. Last-writer-wins
/// on `(lamport, actor)`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Reorder {
    /// Step being reordered.
    pub id: StepId,
    /// New Logoot position key.
    pub key: LogootKey,
}

/// `LeaseStep(step, expires_at_ms)` — request a worker lease on a step (N7.6).
///
/// When two `LeaseStep` ops race the same step, the winner is the envelope
/// with the lexicographically larger `(lamport, actor)` pair: highest lamport
/// wins, ties broken by larger actor id. Expired leases (those whose
/// `expires_at_ms <= now_ms`) are filtered out of
/// [`crate::plan::Plan::lease_holder`] but remain in the underlying fold state
/// so re-folding the log is deterministic regardless of wall-clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LeaseStep {
    /// Step being leased.
    pub step: StepId,
    /// Wall-clock expiry, in milliseconds since the unix epoch (or any other
    /// monotonic reference agreed on by producers). Compared against the
    /// `now_ms` passed to `lease_holder`.
    pub expires_at_ms: u64,
}

/// The CRDT op alphabet.
///
/// Each variant is a small, plain payload — wrapping in an [`OpEnvelope`] is
/// what attaches the Lamport/actor metadata used by the fold.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Op {
    /// See [`AddStep`].
    AddStep(AddStep),
    /// See [`MarkStep`].
    MarkStep(MarkStep),
    /// See [`EditContent`].
    EditContent(EditContent),
    /// See [`AddNote`].
    AddNote(AddNote),
    /// See [`Reorder`].
    Reorder(Reorder),
    /// See [`LeaseStep`].
    LeaseStep(LeaseStep),
    /// See [`Snapshot`] — a persistence-layer fast-forward marker (P9.3,
    /// N7.7). Folding a `Snapshot` op is a no-op: snapshots restore state by
    /// loading the CAS-stored body directly via
    /// [`crate::store::PlanStore::load_latest_snapshot`], bypassing the fold.
    Snapshot(Snapshot),
}

impl Op {
    /// Stable discriminator used as a final tie-breaker in fold ordering when
    /// two envelopes share the same `(lamport, actor)` — a degenerate input
    /// that should never happen in practice but is well-defined here for
    /// total ordering robustness.
    #[must_use]
    pub const fn kind_discriminator(&self) -> u8 {
        match self {
            Self::AddStep(_) => 0,
            Self::MarkStep(_) => 1,
            Self::EditContent(_) => 2,
            Self::AddNote(_) => 3,
            Self::Reorder(_) => 4,
            Self::LeaseStep(_) => 5,
            Self::Snapshot(_) => 6,
        }
    }

    /// Short stable tag stored in the V4 `plan_ops.op_kind` column.
    /// Used only as a human/diagnostic label; the canonical type info is in
    /// `body` (bincode-encoded `OpEnvelope`).
    #[must_use]
    pub const fn kind_tag(&self) -> &'static str {
        match self {
            Self::AddStep(_) => "AddStep",
            Self::MarkStep(_) => "MarkStep",
            Self::EditContent(_) => "EditContent",
            Self::AddNote(_) => "AddNote",
            Self::Reorder(_) => "Reorder",
            Self::LeaseStep(_) => "LeaseStep",
            Self::Snapshot(_) => "Snapshot",
        }
    }
}

/// One entry in the op log: producer + clock + payload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OpEnvelope {
    /// Producing actor.
    pub actor: ActorId,
    /// Lamport timestamp.
    pub lamport: Lamport,
    /// The op itself.
    pub op: Op,
}

impl OpEnvelope {
    /// Construct an envelope.
    #[must_use]
    pub const fn new(actor: ActorId, lamport: Lamport, op: Op) -> Self {
        Self { actor, lamport, op }
    }

    /// Total-order key — used as the sort key by [`crate::fold::fold`].
    #[must_use]
    pub const fn key(&self) -> OpKey {
        OpKey::new(self.lamport, self.actor)
    }
}
