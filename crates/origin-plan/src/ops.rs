//! Op-log alphabet for the shared plan CRDT (P9.1).
//!
//! Every op is wrapped in an [`OpEnvelope`] carrying the producing
//! [`ActorId`] and a [`Lamport`] timestamp. The pair `(lamport, actor)` is the
//! canonical total order under which [`crate::fold::fold`] folds the log into
//! a deterministic [`crate::plan::Plan`].

use crate::lamport::{ActorId, Lamport, OpKey};
use crate::logoot::LogootKey;

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

/// Lifecycle status of a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkStep {
    /// Step being marked.
    pub id: StepId,
    /// New status.
    pub status: Status,
}

/// `EditContent(id, body)` — last-writer-wins replace of a step body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditContent {
    /// Step being edited.
    pub id: StepId,
    /// New body.
    pub body: String,
}

/// `AddNote(id, body)` — append a note to a step. Notes form an ordered list
/// driven by `(lamport, actor)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddNote {
    /// Step the note is attached to.
    pub id: StepId,
    /// Note body.
    pub body: String,
}

/// `Reorder(id, key)` — move a step to a new Logoot position. Last-writer-wins
/// on `(lamport, actor)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reorder {
    /// Step being reordered.
    pub id: StepId,
    /// New Logoot position key.
    pub key: LogootKey,
}

/// The CRDT op alphabet.
///
/// Each variant is a small, plain payload — wrapping in an [`OpEnvelope`] is
/// what attaches the Lamport/actor metadata used by the fold.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        }
    }
}

/// One entry in the op log: producer + clock + payload.
#[derive(Debug, Clone, PartialEq, Eq)]
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
