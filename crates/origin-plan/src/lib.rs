//! `origin-plan` — CRDT op log + fold for the shared swarm plan (Phase 9.1).
//!
//! ## Surface
//!
//! - [`Op`] alphabet: `AddStep`, `MarkStep`, `EditContent` (LWW), `AddNote`
//!   (append), `Reorder` (Logoot keys).
//! - [`OpEnvelope`] wraps each op with its `(lamport, actor)` Lamport-clock
//!   coordinates.
//! - [`fold`] folds an op-log iterator into a deterministic [`Plan`]. The fold
//!   is permutation-invariant: any input order yields the same plan because
//!   the fold sorts by `(lamport, actor)` before applying.
//! - [`LogootKey::between`] produces dense, totally-ordered list positions
//!   for `Reorder` without coordinator round-trips.
//!
//! ## Determinism rules
//!
//! - Total order over the op log is `(lamport, actor)`; the op-kind
//!   discriminator is the degenerate tie-breaker.
//! - `EditContent`, `MarkStep`, and `Reorder` are last-writer-wins on that
//!   key — each step tracks the highest key seen for each field.
//! - `AddNote` appends in fold order — so notes are stably sorted by
//!   `(lamport, actor)`.
//! - `AddStep` is first-writer-wins on `StepId` (duplicate ids are a producer
//!   bug, not a fold-time failure).
//! - Ops referencing an unknown `StepId` are dropped — when the missing
//!   `AddStep` eventually arrives, re-folding the now-complete log produces
//!   the correct state.
//!
//! This module is the substrate for P9.2 lease tokens (Lamport ordering)
//! and P9.3 snapshot compaction.

pub mod fold;
pub mod lamport;
pub mod lease;
pub mod logoot;
pub mod ops;
pub mod plan;

pub use fold::fold;
pub use lamport::{ActorId, Lamport, OpKey};
pub use lease::{LeaseOutcome, LeaseRecord};
pub use logoot::{LogootKey, PathComponent};
pub use ops::{AddNote, AddStep, EditContent, LeaseStep, MarkStep, Op, OpEnvelope, Reorder, Status, StepId};
pub use plan::{Plan, Step};
