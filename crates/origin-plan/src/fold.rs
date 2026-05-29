// SPDX-License-Identifier: Apache-2.0
//! Op-log → [`Plan`] fold (P9.1).
//!
//! `fold` is the canonical CRDT projection. It is **commutative under
//! permutation of input ops** — any reordering of the input iterator produces
//! an identical [`Plan`] — because the very first step is to sort envelopes by
//! `(lamport, actor)`. After that the apply loop is straight-line.

use crate::lease::LeaseRecord;
use crate::ops::{Op, OpEnvelope};
use crate::plan::{Plan, Step};

/// Fold an op-log iterator into a deterministic [`Plan`].
///
/// The fold sorts envelopes by `(lamport, actor)`, with the op-kind
/// discriminator as a degenerate tie-breaker, then applies them in order.
/// Drop-on-floor semantics for ops that reference an unknown [`StepId`] —
/// this can happen mid-stream while a peer hasn't yet delivered the
/// corresponding `AddStep`; once it does, replaying the now-complete log
/// yields the right state.
#[must_use]
pub fn fold<I: IntoIterator<Item = OpEnvelope>>(envs: I) -> Plan {
    let mut buf: Vec<OpEnvelope> = envs.into_iter().collect();
    buf.sort_by(|a, b| {
        a.key()
            .cmp(&b.key())
            .then_with(|| a.op.kind_discriminator().cmp(&b.op.kind_discriminator()))
    });

    let mut plan = Plan::new();
    for env in buf {
        let op_key = env.key();
        match env.op {
            Op::AddStep(add) => {
                let step = Step::from_add(add.id, add.parent, add.body, op_key, add.key.clone(), op_key);
                plan.insert(step);
            }
            Op::MarkStep(mark) => {
                if let Some(s) = plan.get_mut(mark.id) {
                    s.apply_mark(mark.status, op_key);
                }
            }
            Op::EditContent(edit) => {
                if let Some(s) = plan.get_mut(edit.id) {
                    s.apply_edit(edit.body, op_key);
                }
            }
            Op::AddNote(note) => {
                if let Some(s) = plan.get_mut(note.id) {
                    s.push_note(note.body);
                }
            }
            Op::Reorder(re) => {
                if let Some(s) = plan.get_mut(re.id) {
                    s.apply_reorder(re.key, op_key);
                }
            }
            Op::LeaseStep(lease) => {
                // Lease records are infallible: races are resolved by
                // `(lamport, actor)` lex order via `LeaseRecord::supersedes`.
                // Lease ops for unknown steps are dropped (mirrors the rest
                // of the alphabet).
                let candidate = LeaseRecord::new(env.lamport, env.actor, lease.expires_at_ms);
                plan.apply_lease(lease.step, candidate);
            }
            Op::Snapshot(_) => {
                // Snapshots are a persistence-layer fast-forward marker
                // (P9.3, N7.7). Folding one does not change `Plan` state:
                // snapshot restoration goes through
                // `PlanStore::load_latest_snapshot`, which deserializes the
                // CAS-stored body directly and bypasses the fold.
            }
        }
    }

    plan
}

#[cfg(test)]
mod tests {
    use super::fold;
    use crate::lamport::{ActorId, Lamport};
    use crate::logoot::LogootKey;
    use crate::ops::{AddStep, Op, OpEnvelope, StepId};

    #[test]
    fn empty_log_yields_empty_plan() {
        let plan = fold(std::iter::empty());
        assert!(plan.is_empty());
    }

    #[test]
    fn add_step_then_get() {
        let actor = ActorId::new(1);
        let id = StepId::from_u128(42);
        let key = LogootKey::between(None, None, actor, 1);
        let env = OpEnvelope::new(
            actor,
            Lamport::new(1),
            Op::AddStep(AddStep {
                id,
                parent: None,
                body: "hello".into(),
                key,
            }),
        );
        let plan = fold(std::iter::once(env));
        assert_eq!(plan.get(id).expect("present").body(), "hello");
    }

    #[test]
    fn unknown_id_ops_are_dropped() {
        let actor = ActorId::new(1);
        let id = StepId::from_u128(7);
        // Only an EditContent — no AddStep first. Nothing should land.
        let env = OpEnvelope::new(
            actor,
            Lamport::new(1),
            Op::EditContent(crate::ops::EditContent {
                id,
                body: "ghost".into(),
            }),
        );
        let plan = fold(std::iter::once(env));
        assert!(plan.is_empty());
    }
}
