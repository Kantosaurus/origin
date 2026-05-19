//! Snapshot-compaction tests for `origin-plan` (P9.3, N7.7).
//!
//! `PlanStore::write_snapshot` is required to:
//! 1. Store the serialized `Plan` body in the CAS.
//! 2. Insert a row into `plan_snapshots` keyed by `seq` (= lamport of the
//!    snapshot op) with `state_handle` and `fully_acked_below`.
//! 3. Delete all rows from `plan_ops` whose `lamport < fully_acked_below`.
//!
//! After compaction, `load_log` returns only the surviving ops (those with
//! `lamport >= latest_snapshot.fully_acked_below`).

use origin_cas::{Store as CasStore, StoreConfig};
use origin_plan::{
    fold, ActorId, AddStep, Lamport, LogootKey, Op, OpEnvelope, Plan, PlanStore, Snapshot, StepId,
};
use std::sync::Arc;
use tempfile::TempDir;

fn open_cas(root: std::path::PathBuf) -> CasStore {
    CasStore::open(StoreConfig {
        root,
        hot_capacity: 64,
        warm_pack_target_bytes: 1 << 20,
        cold_zstd_level: 3,
    })
    .expect("open cas")
}

#[test]
fn snapshot_gcs_ops_below_acked_seq() {
    let tmp = TempDir::new().expect("tmp");
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
    let cas = Arc::new(open_cas(tmp.path().join("cas")));
    let ps = PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).expect("plan store");

    // Build 200 AddStep ops with lamports 1..=200.
    let actor = ActorId::new(1);
    let mut envs: Vec<OpEnvelope> = Vec::with_capacity(200);
    let mut prev_key: Option<LogootKey> = None;
    for i in 1u64..=200 {
        let id = StepId::from_u128(u128::from(i));
        let key = LogootKey::between(prev_key.as_ref(), None, actor, i);
        envs.push(OpEnvelope::new(
            actor,
            Lamport::new(i),
            Op::AddStep(AddStep {
                id,
                parent: None,
                body: format!("step-{i}"),
                key: key.clone(),
            }),
        ));
        prev_key = Some(key);
    }
    for env in &envs {
        ps.append_op(env).expect("append op");
    }
    let plan = fold(envs.iter().cloned());

    // Take a snapshot acking everything below lamport 150.
    let body = plan.serialize_for_snapshot();
    let handle = cas.put(&body).expect("cas put");
    let snap = Snapshot {
        seq: 201,
        state_handle: *handle.as_bytes(),
        fully_acked_below: 150,
    };
    ps.write_snapshot(&snap, &body).expect("write snap");

    // load_log should now return ops with lamport >= 150 only.
    let loaded = ps.load_log().expect("load log");
    assert!(
        loaded.iter().all(|e| e.lamport.value() >= 150),
        "found op below fully_acked_below"
    );
    // Ops 150..=200 = 51 entries.
    assert_eq!(
        loaded.len(),
        51,
        "expected 51 surviving ops, got {}",
        loaded.len()
    );
}

#[test]
fn load_latest_snapshot_round_trips() {
    let tmp = TempDir::new().expect("tmp");
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
    let cas = Arc::new(open_cas(tmp.path().join("cas")));
    let ps = PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).expect("plan store");

    let actor = ActorId::new(7);
    let id = StepId::from_u128(0xFEED);
    let key = LogootKey::between(None, None, actor, 1);
    let op = OpEnvelope::new(
        actor,
        Lamport::new(1),
        Op::AddStep(AddStep {
            id,
            parent: None,
            body: "only-step".into(),
            key,
        }),
    );
    let plan: Plan = fold(std::iter::once(op.clone()));
    ps.append_op(&op).expect("append op");

    let body = plan.serialize_for_snapshot();
    let handle = cas.put(&body).expect("cas put");
    let snap = Snapshot {
        seq: 1,
        state_handle: *handle.as_bytes(),
        fully_acked_below: 1,
    };
    ps.write_snapshot(&snap, &body).expect("write snap");

    let (loaded_snap, loaded_plan) = ps
        .load_latest_snapshot()
        .expect("load snap result")
        .expect("snapshot present");
    assert_eq!(loaded_snap.seq, 1);
    assert_eq!(loaded_plan.len(), 1);
    assert_eq!(loaded_plan.get(id).expect("step present").body(), "only-step");
}
