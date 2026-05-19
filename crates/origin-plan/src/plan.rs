//! Materialised plan state — the result of folding the op log.
//!
//! [`Plan`] is the post-fold view: a flat map of [`StepId`] → [`Step`] plus a
//! root list. Construction is gated behind [`crate::fold::fold`] so all
//! state transitions go through the same deterministic path.

use std::collections::{BTreeMap, HashMap};

use crate::lamport::{ActorId, OpKey};
use crate::lease::{LeaseOutcome, LeaseRecord};
use crate::logoot::LogootKey;
use crate::ops::{Op, OpEnvelope, Status, StepId};

/// A single step inside a [`Plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    id: StepId,
    parent: Option<StepId>,
    body: String,
    body_lww: OpKey,
    status: Status,
    status_lww: OpKey,
    notes: Vec<String>,
    key: LogootKey,
    key_lww: OpKey,
}

impl Step {
    /// Internal constructor used by the fold. Public for downstream tests in
    /// `tests/`.
    #[must_use]
    pub(crate) const fn from_add(
        id: StepId,
        parent: Option<StepId>,
        body: String,
        body_lww: OpKey,
        key: LogootKey,
        key_lww: OpKey,
    ) -> Self {
        Self {
            id,
            parent,
            body,
            body_lww,
            status: Status::Pending,
            status_lww: OpKey::new(crate::lamport::Lamport::ZERO, crate::lamport::ActorId::new(0)),
            notes: Vec::new(),
            key,
            key_lww,
        }
    }

    /// Stable id.
    #[must_use]
    pub const fn id(&self) -> StepId {
        self.id
    }

    /// Parent step (root list if `None`).
    #[must_use]
    pub const fn parent(&self) -> Option<StepId> {
        self.parent
    }

    /// Current body after LWW resolution.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Current status after LWW resolution.
    #[must_use]
    pub const fn status(&self) -> Status {
        self.status
    }

    /// Appended notes, in `(lamport, actor)` order.
    #[must_use]
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// Current Logoot position key.
    #[must_use]
    pub const fn key(&self) -> &LogootKey {
        &self.key
    }

    pub(crate) fn apply_edit(&mut self, body: String, key: OpKey) {
        if key > self.body_lww {
            self.body = body;
            self.body_lww = key;
        }
    }

    pub(crate) fn apply_mark(&mut self, status: Status, key: OpKey) {
        if key > self.status_lww {
            self.status = status;
            self.status_lww = key;
        }
    }

    pub(crate) fn apply_reorder(&mut self, new_key: LogootKey, key: OpKey) {
        if key > self.key_lww {
            self.key = new_key;
            self.key_lww = key;
        }
    }

    pub(crate) fn push_note(&mut self, body: String) {
        self.notes.push(body);
    }
}

/// Folded plan state — a deterministic projection of the op log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    steps: BTreeMap<StepId, Step>,
    /// Per-step lease holders. A record is the canonical winner of all
    /// `LeaseStep` ops seen so far for its step, ordered by
    /// `(lamport, actor.value())`. Expired leases stay in the map for fold
    /// determinism; [`Plan::lease_holder`] filters them with `now_ms`.
    leases: HashMap<StepId, LeaseRecord>,
}

impl Plan {
    /// Construct an empty plan. Equivalent to [`Default::default`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            steps: BTreeMap::new(),
            leases: HashMap::new(),
        }
    }

    /// Step count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// `true` if no steps have been inserted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Look up a step by id.
    #[must_use]
    pub fn get(&self, id: StepId) -> Option<&Step> {
        self.steps.get(&id)
    }

    /// Iterate root-level steps in Logoot order.
    pub fn iter_root(&self) -> impl Iterator<Item = &Step> {
        self.iter_children(None)
    }

    /// Iterate the direct children of `parent` in Logoot order. `parent =
    /// None` iterates the root list.
    pub fn iter_children(&self, parent: Option<StepId>) -> impl Iterator<Item = &Step> {
        let mut v: Vec<&Step> = self.steps.values().filter(|s| s.parent == parent).collect();
        v.sort_by(|a, b| a.key.cmp(&b.key));
        v.into_iter()
    }

    /// Current canonical lease holder for `step`, if any non-expired lease op
    /// has been folded for it.
    ///
    /// Returns `None` if no lease has been folded for the step, *or* if the
    /// canonical lease has `expires_at_ms <= now_ms`. Pass `now_ms = 0` (or
    /// any small value) to bypass expiry filtering. Expired records remain in
    /// the underlying state so re-folding the log is wall-clock independent.
    #[must_use]
    pub fn lease_holder(&self, step: StepId, now_ms: u64) -> Option<ActorId> {
        self.leases
            .get(&step)
            .filter(|r| r.expires_at_ms > now_ms)
            .map(|r| r.actor)
    }

    /// Query the fate of an individual `LeaseStep` op against the current
    /// fold state.
    ///
    /// - Non-lease ops yield [`LeaseOutcome::NotALease`].
    /// - Lease ops whose `(lamport, actor)` matches the canonical holder
    ///   yield [`LeaseOutcome::Granted`].
    /// - Lease ops that lost a race yield [`LeaseOutcome::Lost`] with the
    ///   winning actor.
    /// - Lease ops whose step has no folded record (e.g. the corresponding
    ///   `AddStep` never arrived, so the lease op was dropped during fold)
    ///   also yield [`LeaseOutcome::NotALease`] — there is no winner to report.
    ///
    /// `lease_outcome` reflects fold state, not wall-clock — expired but
    /// canonical leases still report `Granted` so callers can distinguish
    /// "lost the race" from "lease expired".
    #[must_use]
    pub fn lease_outcome(&self, op: &OpEnvelope) -> LeaseOutcome {
        let Op::LeaseStep(lease) = &op.op else {
            return LeaseOutcome::NotALease;
        };
        match self.leases.get(&lease.step) {
            Some(rec) if rec.lamport == op.lamport && rec.actor == op.actor => {
                LeaseOutcome::Granted { holder: rec.actor }
            }
            Some(rec) => LeaseOutcome::Lost { winner: rec.actor },
            None => LeaseOutcome::NotALease,
        }
    }

    /// Iterate `(step, record)` pairs for every lease still held in fold
    /// state. Includes expired records; callers can filter with `now_ms` as
    /// needed.
    pub fn leases(&self) -> impl Iterator<Item = (StepId, &LeaseRecord)> + '_ {
        self.leases.iter().map(|(k, v)| (*k, v))
    }

    pub(crate) fn apply_lease(&mut self, step: StepId, candidate: LeaseRecord) {
        // Only install a record if the step exists in the fold; lease ops for
        // unknown steps are dropped, matching the rest of the alphabet.
        if !self.steps.contains_key(&step) {
            return;
        }
        match self.leases.get(&step) {
            Some(existing) if !candidate.supersedes(existing) => {}
            _ => {
                self.leases.insert(step, candidate);
            }
        }
    }

    pub(crate) fn insert(&mut self, step: Step) {
        // First writer wins on AddStep: if a duplicate id is somehow added by
        // another actor (a producer bug), keep the earlier op's data. The
        // fold orders by (lamport, actor), so the first envelope to land is
        // the canonical one. Later AddSteps for the same id are ignored
        // rather than overwriting LWW state from edits/marks that may have
        // already arrived.
        self.steps.entry(step.id).or_insert(step);
    }

    pub(crate) fn get_mut(&mut self, id: StepId) -> Option<&mut Step> {
        self.steps.get_mut(&id)
    }
}
