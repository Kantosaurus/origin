//! Property test for `origin-plan` fold determinism (P9.1).
//!
//! The CRDT correctness invariant: any permutation of the op-log must fold to
//! the same `Plan` state. The fold function internally orders ops by
//! `(lamport, actor)`; clients may deliver them in any order.

use std::collections::HashSet;

use origin_plan::{
    fold, ActorId, AddNote, AddStep, EditContent, Lamport, LogootKey, MarkStep, Op, OpEnvelope, Plan,
    Reorder, Status, StepId,
};
use proptest::collection::vec;
use proptest::prelude::*;

fn arb_actor() -> impl Strategy<Value = ActorId> {
    (1u64..=4).prop_map(ActorId::new)
}

fn arb_status() -> impl Strategy<Value = Status> {
    prop_oneof![
        Just(Status::Pending),
        Just(Status::InProgress),
        Just(Status::Done),
        Just(Status::Cancelled),
    ]
}

/// Produce a syntactically valid op-log: every non-`AddStep` op references an
/// id that was introduced by some earlier `AddStep`.
fn arb_op_log() -> impl Strategy<Value = Vec<OpEnvelope>> {
    // Stage 1: build an arbitrary list of (kind, actor, payload) descriptors.
    // Stage 2: in a deterministic post-pass we assign Lamport timestamps and
    //          rewrite step-id references onto existing ids.
    let descriptor = (
        0u8..=4,       // op kind
        arb_actor(),   // actor
        any::<u32>(),  // id seed
        "[a-z]{1,8}",  // text body
        arb_status(),  // for MarkStep
        0u64..1024,    // logoot position seed
        any::<bool>(), // for Reorder: place at end or relative
    );
    vec(descriptor, 1..40).prop_map(|raw| {
        let mut steps: Vec<StepId> = Vec::new();
        let mut envs: Vec<OpEnvelope> = Vec::with_capacity(raw.len());
        let mut next_logoot_pos: u64 = 1;
        for (i, (kind, actor, seed, text, status, pos_seed, _flip)) in raw.into_iter().enumerate() {
            let lamport = Lamport::new(i as u64 + 1);
            let op = if steps.is_empty() || kind == 0 {
                // Force initial op and any time we need a step to be AddStep.
                let id = StepId::from_u128(u128::from(seed) ^ (u128::from(lamport.value()) << 32));
                steps.push(id);
                let key = LogootKey::between(None, None, actor, next_logoot_pos);
                next_logoot_pos += 1;
                Op::AddStep(AddStep {
                    id,
                    parent: None,
                    body: text,
                    key,
                })
            } else {
                let target_idx = (seed as usize) % steps.len();
                let id = steps[target_idx];
                match kind {
                    1 => Op::MarkStep(MarkStep { id, status }),
                    2 => Op::EditContent(EditContent { id, body: text }),
                    3 => Op::AddNote(AddNote { id, body: text }),
                    _ => {
                        let key = LogootKey::between(None, None, actor, pos_seed.saturating_add(1));
                        Op::Reorder(Reorder { id, key })
                    }
                }
            };
            envs.push(OpEnvelope::new(actor, lamport, op));
        }
        envs
    })
}

fn shuffled(envs: &[OpEnvelope], seed: u64) -> Vec<OpEnvelope> {
    // Simple deterministic shuffle: xorshift over a permutation index list.
    let mut out: Vec<(u64, OpEnvelope)> = envs
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let mut x = seed.wrapping_add(i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            x ^= x >> 30;
            x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
            x ^= x >> 27;
            (x, e.clone())
        })
        .collect();
    out.sort_by_key(|(k, _)| *k);
    out.into_iter().map(|(_, e)| e).collect()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        .. ProptestConfig::default()
    })]

    /// Folding any permutation of the same op-log yields the same Plan.
    #[test]
    fn fold_is_permutation_invariant(envs in arb_op_log()) {
        let canonical: Plan = fold(envs.iter().cloned());
        for seed in [1u64, 7, 42, 2024, 0xDEAD_BEEF] {
            let perm = shuffled(&envs, seed);
            let folded: Plan = fold(perm);
            prop_assert_eq!(&folded, &canonical);
        }
    }

    /// Step iteration order is deterministic and matches the Logoot ordering.
    #[test]
    fn iter_order_is_sorted_by_logoot(envs in arb_op_log()) {
        let plan = fold(envs);
        let keys: Vec<LogootKey> = plan.iter_root().map(|s| s.key().clone()).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        prop_assert_eq!(keys, sorted);
    }

    /// No duplicate step ids in the folded plan.
    #[test]
    fn no_duplicate_step_ids(envs in arb_op_log()) {
        let plan = fold(envs);
        let mut seen: HashSet<StepId> = HashSet::new();
        for step in plan.iter_root() {
            prop_assert!(seen.insert(step.id()), "duplicate step id {:?}", step.id());
        }
    }
}

#[test]
fn lww_edit_picks_higher_lamport() {
    let actor_a = ActorId::new(1);
    let actor_b = ActorId::new(2);
    let id = StepId::from_u128(0xCAFE_BABE);
    let key = LogootKey::between(None, None, actor_a, 1);
    let envs = vec![
        OpEnvelope::new(
            actor_a,
            Lamport::new(1),
            Op::AddStep(AddStep {
                id,
                parent: None,
                body: "initial".to_string(),
                key,
            }),
        ),
        OpEnvelope::new(
            actor_a,
            Lamport::new(2),
            Op::EditContent(EditContent {
                id,
                body: "edit-A".to_string(),
            }),
        ),
        OpEnvelope::new(
            actor_b,
            Lamport::new(3),
            Op::EditContent(EditContent {
                id,
                body: "edit-B".to_string(),
            }),
        ),
    ];
    let plan = fold(envs);
    let step = plan.get(id).expect("step should exist after AddStep");
    assert_eq!(step.body(), "edit-B");
}

#[test]
fn notes_append_in_lamport_order() {
    let actor = ActorId::new(1);
    let id = StepId::from_u128(7);
    let key = LogootKey::between(None, None, actor, 1);
    let envs = vec![
        OpEnvelope::new(
            actor,
            Lamport::new(1),
            Op::AddStep(AddStep {
                id,
                parent: None,
                body: "step".into(),
                key,
            }),
        ),
        OpEnvelope::new(
            actor,
            Lamport::new(3),
            Op::AddNote(AddNote {
                id,
                body: "third".into(),
            }),
        ),
        OpEnvelope::new(
            actor,
            Lamport::new(2),
            Op::AddNote(AddNote {
                id,
                body: "second".into(),
            }),
        ),
    ];
    let plan = fold(envs);
    let step = plan.get(id).expect("step exists");
    assert_eq!(
        step.notes().iter().map(String::as_str).collect::<Vec<_>>(),
        vec!["second", "third"]
    );
}

#[test]
fn mark_step_lww_uses_lamport() {
    let actor = ActorId::new(1);
    let id = StepId::from_u128(11);
    let key = LogootKey::between(None, None, actor, 1);
    let envs = vec![
        OpEnvelope::new(
            actor,
            Lamport::new(1),
            Op::AddStep(AddStep {
                id,
                parent: None,
                body: "x".into(),
                key,
            }),
        ),
        OpEnvelope::new(
            actor,
            Lamport::new(5),
            Op::MarkStep(MarkStep {
                id,
                status: Status::Done,
            }),
        ),
        OpEnvelope::new(
            actor,
            Lamport::new(3),
            Op::MarkStep(MarkStep {
                id,
                status: Status::InProgress,
            }),
        ),
    ];
    let plan = fold(envs);
    let step = plan.get(id).expect("exists");
    assert_eq!(step.status(), Status::Done);
}

#[test]
fn reorder_updates_logoot_key() {
    let actor = ActorId::new(1);
    let id_a = StepId::from_u128(100);
    let id_b = StepId::from_u128(200);
    let key_a = LogootKey::between(None, None, actor, 1);
    let key_b = LogootKey::between(None, None, actor, 2);
    let envs = vec![
        OpEnvelope::new(
            actor,
            Lamport::new(1),
            Op::AddStep(AddStep {
                id: id_a,
                parent: None,
                body: "a".into(),
                key: key_a.clone(),
            }),
        ),
        OpEnvelope::new(
            actor,
            Lamport::new(2),
            Op::AddStep(AddStep {
                id: id_b,
                parent: None,
                body: "b".into(),
                key: key_b,
            }),
        ),
        OpEnvelope::new(
            actor,
            Lamport::new(3),
            Op::Reorder(Reorder {
                id: id_b,
                key: LogootKey::between(None, Some(&key_a), actor, 1),
            }),
        ),
    ];
    let plan = fold(envs);
    let order: Vec<StepId> = plan.iter_root().map(origin_plan::Step::id).collect();
    // After reorder, b is placed before a.
    assert_eq!(order, vec![id_b, id_a]);
}
