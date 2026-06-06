// SPDX-License-Identifier: Apache-2.0
//! The coordinator runs spawned workers in PARALLEL — the primitive the agent
//! loop relies on to run multiple `Task` sub-agents at once. Spawning several
//! workers before awaiting any must let them overlap, not run serially.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use origin_cas::{Store as CasStore, StoreConfig};
use origin_plan::{ActorId, Plan, PlanStore};
use origin_swarm::{
    AdmissionGate, Budget, CompletionReport, Coordinator, PlanHandle, ReportStatus, Usage, WorkerContext,
    WorkerFn, WorkerSpec,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawned_workers_run_concurrently_not_serially() {
    let tmp = TempDir::new().expect("tmp");
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
    let cas = Arc::new(open_cas(tmp.path().join("cas")));
    let plan_store = Arc::new(PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).expect("plan store"));
    let plan = PlanHandle::new(Arc::new(Mutex::new(Plan::default())), plan_store);
    // Inject an unlimited gate so this test isolates EXECUTION concurrency from
    // the memory-admission policy (covered separately in tests/admission.rs).
    let coord = Coordinator::new(plan, format!("origin-swarm-conc-{}", std::process::id()))
        .with_memory_gate(AdmissionGate::unlimited_for_test());

    // Each worker bumps a shared "currently running" counter, records the max
    // ever seen, then holds the slot long enough that all three overlap.
    let current = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));
    let (cur2, max2) = (Arc::clone(&current), Arc::clone(&max_seen));
    let worker: WorkerFn = Arc::new(move |ctx: WorkerContext| {
        let (cur, mx) = (Arc::clone(&cur2), Arc::clone(&max2));
        Box::pin(async move {
            let now = cur.fetch_add(1, Ordering::SeqCst) + 1;
            mx.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(80)).await;
            cur.fetch_sub(1, Ordering::SeqCst);
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

    // Spawn three WITHOUT awaiting — each starts running immediately on the pool.
    let h1 = coord
        .spawn_with(spec("a"), Arc::clone(&worker))
        .await
        .expect("spawn a");
    let h2 = coord
        .spawn_with(spec("b"), Arc::clone(&worker))
        .await
        .expect("spawn b");
    let h3 = coord
        .spawn_with(spec("c"), Arc::clone(&worker))
        .await
        .expect("spawn c");
    // Awaiting in sequence still resolves all three concurrently (they overlap).
    coord.await_completion(&h1).await.expect("a done");
    coord.await_completion(&h2).await.expect("b done");
    coord.await_completion(&h3).await.expect("c done");

    assert_eq!(
        max_seen.load(Ordering::SeqCst),
        3,
        "all three sub-agents must run at the same time, not one-at-a-time"
    );
}
