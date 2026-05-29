// SPDX-License-Identifier: Apache-2.0
//! `PrefixLedger` inheritance tests (P9.7, N7.1).
//!
//! Two scenarios:
//! 1. `snapshot_round_trips_band_assignments` — purely synchronous: populate a
//!    `PrefixLedger` with `Frozen`/`Sticky` (and one `Volatile`) entries,
//!    snapshot via `Coordinator::take_prefix_snapshot`, `seed_into` a fresh
//!    ledger, assert `suggested_band` reproduces the stable bands and skips
//!    the non-stable one.
//! 2. `worker_sees_inherited_ledger_in_context` — async: a coordinator built
//!    with `with_parent_ledger` hands every spawned worker the same snapshot
//!    via `WorkerContext::inherited_ledger`.

use std::sync::Arc;

use origin_cas::{Store as CasStore, StoreConfig};
use origin_plan::{ActorId, Plan, PlanStore};
use origin_planner::{Band, PrefixLedger, SectionId};
use origin_swarm::{
    Budget, CompletionReport, Coordinator, PlanHandle, PrefixSnapshot, ReportStatus, Usage, WorkerContext,
    WorkerSpec,
};
use tempfile::TempDir;
use tokio::sync::Mutex;

fn open_cas(root: std::path::PathBuf) -> CasStore {
    CasStore::open(StoreConfig {
        root,
        hot_capacity: 64,
        warm_pack_target_bytes: 1 << 20,
        cold_zstd_level: 3,
    })
    .expect("open cas")
}

const SYS: SectionId = SectionId::new("system");
const SKILL: SectionId = SectionId::new("skill");
const TURN: SectionId = SectionId::new("turn");

#[test]
fn snapshot_round_trips_band_assignments() {
    let mut parent = PrefixLedger::new();
    parent.record_band(SYS, Band::Frozen);
    parent.record_band(SKILL, Band::Sticky);
    parent.record_band(TURN, Band::Volatile);

    let snap = Coordinator::take_prefix_snapshot(&parent);
    assert_eq!(snap.len(), 2, "only Frozen+Sticky entries survive");
    assert!(!snap.is_empty());

    let mut fresh = PrefixLedger::new();
    snap.seed_into(&mut fresh);

    assert_eq!(fresh.suggested_band(SYS), Some(Band::Frozen));
    assert_eq!(fresh.suggested_band(SKILL), Some(Band::Sticky));
    assert_eq!(fresh.suggested_band(TURN), None, "Volatile is not inherited");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_sees_inherited_ledger_in_context() {
    let tmp = TempDir::new().expect("tmp");
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
    let cas = Arc::new(open_cas(tmp.path().join("cas")));
    let plan_store = Arc::new(PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).expect("plan store"));
    let plan = PlanHandle::new(Arc::new(Mutex::new(Plan::default())), plan_store);

    let mut parent_ledger = PrefixLedger::new();
    parent_ledger.record_band(SYS, Band::Frozen);
    parent_ledger.record_band(SKILL, Band::Sticky);
    parent_ledger.record_band(TURN, Band::Volatile);

    let coord = Coordinator::new(plan, format!("origin-swarm-pi-{}", std::process::id()))
        .with_parent_ledger(parent_ledger);

    let observed: Arc<std::sync::Mutex<Option<PrefixSnapshot>>> = Arc::new(std::sync::Mutex::new(None));
    let observed_clone = Arc::clone(&observed);
    let worker: origin_swarm::WorkerFn = Arc::new(move |ctx: WorkerContext| {
        let observed_clone = Arc::clone(&observed_clone);
        Box::pin(async move {
            let snap = ctx.inherited_ledger().clone();
            *observed_clone.lock().expect("lock") = Some(snap);
            Ok(CompletionReport {
                goal: ctx.spec.goal.clone(),
                status: ReportStatus::Completed,
                plan_updates: Vec::new(),
                files_touched: Vec::new(),
                decisions: Vec::new(),
                follow_ups: Vec::new(),
                transcript_handle: [0; 32],
                usage: Usage::default(),
            })
        })
    });

    let spec = WorkerSpec {
        goal: "inherit-test".into(),
        allowed_tools: vec!["read".into()],
        budget: Budget {
            max_wall_ms: 1_000,
            max_input_tokens: 100,
            max_output_tokens: 100,
            max_tool_calls: 1,
        },
        workspace: None,
        parent_actor: ActorId::new(0),
    };

    let handle = coord.spawn_with(spec, worker).await.expect("spawn");
    let report = coord.await_completion(&handle).await.expect("await");
    assert!(matches!(report.status, ReportStatus::Completed));

    let snap = observed
        .lock()
        .expect("lock")
        .clone()
        .expect("worker captured snapshot");
    assert_eq!(snap.len(), 2);

    let mut fresh = PrefixLedger::new();
    snap.seed_into(&mut fresh);
    assert_eq!(fresh.suggested_band(SYS), Some(Band::Frozen));
    assert_eq!(fresh.suggested_band(SKILL), Some(Band::Sticky));
    assert_eq!(fresh.suggested_band(TURN), None);
}
