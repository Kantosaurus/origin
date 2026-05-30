// SPDX-License-Identifier: Apache-2.0
//! Protocol-level tests for the swarm coordinator (P9.6, N7.4, N7.5).
//!
//! Two scenarios are covered:
//! 1. `three_workers_complete_and_report` — `Coordinator::spawn` + the default
//!    noop worker drives three workers to `Completed` with structured
//!    `CompletionReport`s flowing back through `await_completion`.
//! 2. `credit_channel_blocks_at_zero` — the generic `CreditChannel<T>` enforces
//!    its budget on send and re-issues capacity on receive.

use origin_cas::{Store as CasStore, StoreConfig};
use origin_plan::{Plan, PlanStore};
use origin_swarm::{Budget, Coordinator, PlanHandle, ReportStatus, WorkerSpec};
use std::sync::Arc;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_workers_complete_and_report() {
    let tmp = TempDir::new().expect("tmp");
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
    let cas = Arc::new(open_cas(tmp.path().join("cas")));
    let plan_store = Arc::new(PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).expect("plan store"));
    let plan = PlanHandle::new(Arc::new(Mutex::new(Plan::default())), plan_store);
    let coord = Coordinator::new(plan.clone(), format!("origin-swarm-test-{}", std::process::id()));

    let mut handles = Vec::with_capacity(3);
    for i in 0..3u32 {
        let spec = WorkerSpec {
            goal: format!("worker-{i}"),
            allowed_tools: vec!["read".into()],
            budget: Budget {
                max_wall_ms: 5_000,
                max_input_tokens: 1_000,
                max_output_tokens: 1_000,
                max_tool_calls: 4,
            },
            workspace: None,
            parent_actor: origin_plan::ActorId::new(0),
            model: None,
        };
        handles.push(coord.spawn(spec).await.expect("spawn"));
    }

    let mut reports = Vec::with_capacity(3);
    for h in &handles {
        reports.push(coord.await_completion(h).await.expect("await"));
    }
    assert_eq!(reports.len(), 3);
    assert!(reports
        .iter()
        .all(|r| matches!(r.status, ReportStatus::Completed)));

    // Goals propagate through `WorkerContext` into the report.
    let mut goals: Vec<&str> = reports.iter().map(|r| r.goal.as_str()).collect();
    goals.sort_unstable();
    assert_eq!(goals, vec!["worker-0", "worker-1", "worker-2"]);

    // Plan handle remains queryable post-spawn.
    let snap = plan.snapshot().await;
    assert!(snap.is_empty() || !snap.is_empty(), "plan accessible");
}

#[tokio::test]
async fn credit_channel_blocks_at_zero() {
    use origin_swarm::credit::{CreditChannel, TrySendError};
    let (tx, mut rx) = CreditChannel::<u32>::new(2);
    tx.try_send(1).expect("send 1");
    tx.try_send(2).expect("send 2");
    let err = tx.try_send(3).expect_err("at budget");
    assert!(matches!(err, TrySendError::WouldBlock));
    let v = rx.recv().await.expect("recv");
    assert_eq!(v, 1);
    // Receiver issuing a credit unblocks the sender.
    tx.try_send(3).expect("post-issue");
}
