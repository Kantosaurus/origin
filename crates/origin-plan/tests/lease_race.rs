//! Per-step lease-token tests for `origin-plan` (P9.2, N7.6 step 2).
//!
//! Lease tokens are CRDT-style ops with the same `(lamport, actor)` total
//! order as the rest of the op-log. When two `LeaseStep` ops race the same
//! step, the winner is the lexicographically larger `(lamport, actor.value())`
//! tuple — i.e. highest lamport, ties broken by larger actor id. Expired
//! leases (`expires_at_ms <= now_ms`) are filtered out of `lease_holder` but
//! remain in fold state so replay is deterministic.

use origin_plan::{
    fold, ActorId, AddStep, Lamport, LeaseOutcome, LeaseStep, LogootKey, Op, OpEnvelope, StepId,
};

fn add_step_env(actor: ActorId, lamport: u64, id: StepId) -> OpEnvelope {
    let key = LogootKey::between(None, None, actor, 1);
    OpEnvelope::new(
        actor,
        Lamport::new(lamport),
        Op::AddStep(AddStep {
            id,
            parent: None,
            body: "do the thing".into(),
            key,
        }),
    )
}

const fn lease_env(actor: ActorId, lamport: u64, step: StepId, expires_at_ms: u64) -> OpEnvelope {
    OpEnvelope::new(
        actor,
        Lamport::new(lamport),
        Op::LeaseStep(LeaseStep { step, expires_at_ms }),
    )
}

#[test]
fn higher_lamport_wins_lease() {
    let a = ActorId::new(1);
    let b = ActorId::new(2);
    let step = StepId::from_u128(42);

    let lease_a = lease_env(a, 10, step, 1_000);
    let lease_b = lease_env(b, 11, step, 1_000);

    let plan = fold([add_step_env(a, 1, step), lease_a.clone(), lease_b.clone()]);

    assert_eq!(plan.lease_holder(step, 0), Some(b));
    assert!(matches!(
        plan.lease_outcome(&lease_a),
        LeaseOutcome::Lost { winner } if winner == b
    ));
    assert!(matches!(
        plan.lease_outcome(&lease_b),
        LeaseOutcome::Granted { holder } if holder == b
    ));
}

#[test]
fn equal_lamport_breaks_by_larger_actor_bytes() {
    let a = ActorId::new(1);
    let b = ActorId::new(2);
    let step = StepId::from_u128(42);

    let la = lease_env(a, 10, step, 1_000);
    let lb = lease_env(b, 10, step, 1_000);

    let plan = fold([add_step_env(a, 1, step), la.clone(), lb.clone()]);

    assert_eq!(plan.lease_holder(step, 0), Some(b));
    assert!(matches!(
        plan.lease_outcome(&la),
        LeaseOutcome::Lost { winner } if winner == b
    ));
    assert!(matches!(
        plan.lease_outcome(&lb),
        LeaseOutcome::Granted { holder } if holder == b
    ));
}

#[test]
fn expired_lease_is_not_a_holder() {
    let a = ActorId::new(1);
    let step = StepId::from_u128(42);

    let lease = lease_env(a, 10, step, 100);

    let plan = fold([add_step_env(a, 1, step), lease.clone()]);

    assert_eq!(plan.lease_holder(step, 50), Some(a));
    assert_eq!(plan.lease_holder(step, 200), None);
    // The op is still the canonical winner — `lease_outcome` reflects fold
    // state, not the wall clock, so it stays `Granted` even past expiry.
    assert!(matches!(
        plan.lease_outcome(&lease),
        LeaseOutcome::Granted { holder } if holder == a
    ));
}

#[test]
fn non_lease_op_returns_not_a_lease() {
    let a = ActorId::new(1);
    let step = StepId::from_u128(42);
    let add = add_step_env(a, 1, step);
    let plan = fold([add.clone()]);
    assert!(matches!(plan.lease_outcome(&add), LeaseOutcome::NotALease));
}
