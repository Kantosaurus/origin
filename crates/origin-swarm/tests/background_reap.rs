// SPDX-License-Identifier: Apache-2.0
//! `try_completion` is the NON-BLOCKING reap that lets a background-job registry
//! poll detached workers without parking: `None` while running, `Some(Ok/Err)`
//! once terminal, and `Some(Err)` for an evicted/unknown handle. `evict` prunes
//! the reaped worker so the coordinator's map doesn't grow unbounded.

use std::sync::Arc;

use origin_cas::{Store as CasStore, StoreConfig};
use origin_plan::{ActorId, Plan, PlanStore};
use origin_swarm::{
    AdmissionGate, Budget, CompletionReport, Coordinator, PlanHandle, ReportStatus, Usage,
    WorkerContext, WorkerFn, WorkerSpec,
};
use tempfile::TempDir;
use tokio::sync::{Mutex, Notify};

fn open_cas(root: std::path::PathBuf) -> CasStore {
    CasStore::open(StoreConfig {
        root,
        hot_capacity: 64,
        warm_pack_target_bytes: 1 << 20,
        cold_zstd_level: 3,
    })
    .expect("open cas")
}

fn spec(goal: &str) -> WorkerSpec {
    WorkerSpec {
        goal: goal.into(),
        allowed_tools: vec![],
        budget: Budget {
            max_wall_ms: 5_000,
            max_input_tokens: 100,
            max_output_tokens: 100,
            max_tool_calls: 10,
        },
        workspace: None,
        parent_actor: ActorId::new(0),
        model: None,
        mcp_servers: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn try_completion_polls_without_blocking_then_evicts() {
    let tmp = TempDir::new().expect("tmp");
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
    let cas = Arc::new(open_cas(tmp.path().join("cas")));
    let plan_store = Arc::new(PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).expect("plan store"));
    let plan = PlanHandle::new(Arc::new(Mutex::new(Plan::default())), plan_store);
    let coord = Coordinator::new(plan, format!("origin-swarm-reap-{}", std::process::id()))
        .with_memory_gate(AdmissionGate::unlimited_for_test());

    // The worker signals `started`, parks on `release`, then completes — so the
    // test can deterministically observe the running (non-terminal) state.
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (s2, r2) = (Arc::clone(&started), Arc::clone(&release));
    let worker: WorkerFn = Arc::new(move |ctx: WorkerContext| {
        let (s, r) = (Arc::clone(&s2), Arc::clone(&r2));
        Box::pin(async move {
            s.notify_one();
            r.notified().await;
            Ok(CompletionReport {
                goal: ctx.spec.goal.clone(),
                status: ReportStatus::Completed,
                plan_updates: Vec::new(),
                files_touched: Vec::new(),
                decisions: Vec::new(),
                follow_ups: Vec::new(),
                transcript_handle: [0; 32],
                usage: Usage::default(),
                detail: None,
            })
        })
    });

    let h = coord.spawn_with(spec("bg"), worker).await.expect("spawn");
    started.notified().await; // worker is now running, parked on `release`

    // Still running ⇒ a non-blocking poll returns None (does not park).
    assert!(coord.try_completion(&h).await.is_none(), "running ⇒ None");

    release.notify_one();
    coord.await_completion(&h).await.expect("done"); // ensure terminal

    // Terminal ⇒ Some(Ok(report)).
    let reaped = coord.try_completion(&h).await;
    assert!(
        matches!(reaped, Some(Ok(ref r)) if r.goal == "bg"),
        "terminal ⇒ Some(Ok(report)), got {reaped:?}"
    );

    // After eviction the entry is gone ⇒ Some(Err) (a vanished worker), so a
    // reconcile path resolves it rather than waiting forever.
    coord.evict(&h).await;
    assert!(
        matches!(coord.try_completion(&h).await, Some(Err(_))),
        "evicted ⇒ Some(Err) unknown worker"
    );
}
