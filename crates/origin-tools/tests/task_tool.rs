// SPDX-License-Identifier: Apache-2.0
//! P9.8 — `Task` builtin tool: dispatch a worker, await `CompletionReport`,
//! inline the actionable view.
//!
//! Two scenarios:
//! 1. End-to-end happy path with a synthetic worker reporting `Completed` and a
//!    single `TaskRef` follow-up.
//! 2. The `allowed_tools` list flows through `WorkerSpec` and can be observed
//!    by the worker (captured into a `DecisionRecord`).

use std::sync::Arc;

use origin_cas::{Store as CasStore, StoreConfig};
use origin_plan::{Plan, PlanStore};
use origin_swarm::{CompletionReport, Coordinator, DecisionRecord, PlanHandle, ReportStatus, TaskRef, Usage};
use origin_tools::builtins::task::{task_tool, TaskBudget, TaskInput};
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
async fn task_tool_dispatches_worker_and_inlines_report() {
    let tmp = TempDir::new().expect("tmp");
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
    let cas = Arc::new(open_cas(tmp.path().join("cas")));
    let plan_store = Arc::new(PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).expect("plan store"));
    let plan = PlanHandle::new(Arc::new(Mutex::new(Plan::default())), plan_store);
    let mut coord = Coordinator::new(plan, format!("origin-task-{}", std::process::id()));

    coord.set_default_worker(Arc::new(|ctx| {
        Box::pin(async move {
            Ok(CompletionReport {
                goal: ctx.spec.goal.clone(),
                status: ReportStatus::Completed,
                plan_updates: Vec::new(),
                files_touched: Vec::new(),
                decisions: Vec::new(),
                follow_ups: vec![TaskRef {
                    goal: "next".into(),
                    allowed_tools: vec![],
                }],
                transcript_handle: [0; 32],
                usage: Usage::default(),
            })
        })
    }));

    let out = task_tool(
        &coord,
        TaskInput {
            goal: "do the thing".into(),
            allowed_tools: vec!["read".into()],
            budget: TaskBudget::default(),
            model: None,
            mcp_servers: vec![],
        },
    )
    .await
    .expect("task ok");
    assert_eq!(out.status, "completed");
    assert!(out.summary.contains("do the thing"));
    assert_eq!(out.follow_ups, vec!["next".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_tool_filters_disallowed_tools_into_decision_record() {
    let tmp = TempDir::new().expect("tmp");
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
    let cas = Arc::new(open_cas(tmp.path().join("cas")));
    let plan_store = Arc::new(PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).expect("plan store"));
    let plan = PlanHandle::new(Arc::new(Mutex::new(Plan::default())), plan_store);
    let mut coord = Coordinator::new(plan, format!("origin-task-filter-{}", std::process::id()));

    // Synthetic worker echoes back the allowed_tools it received from the spec
    // as a DecisionRecord. `task_tool` is responsible for forwarding the
    // allow-list verbatim, so observing it here proves the wiring.
    coord.set_default_worker(Arc::new(|ctx| {
        let allowed = ctx.spec.allowed_tools.clone();
        Box::pin(async move {
            Ok(CompletionReport {
                goal: ctx.spec.goal.clone(),
                status: ReportStatus::Completed,
                plan_updates: Vec::new(),
                files_touched: Vec::new(),
                decisions: vec![DecisionRecord {
                    at_lamport: 0,
                    decision: "received_allowed_tools".into(),
                    rationale: allowed.join(","),
                }],
                follow_ups: Vec::new(),
                transcript_handle: [0; 32],
                usage: Usage::default(),
            })
        })
    }));

    let _ = task_tool(
        &coord,
        TaskInput {
            goal: "scoped".into(),
            allowed_tools: vec!["read".into(), "grep".into()],
            budget: TaskBudget::default(),
            model: None,
            mcp_servers: vec![],
        },
    )
    .await
    .expect("task ok");

    let report = coord
        .last_completion_for_test()
        .expect("last completion populated");
    let rec = report.decisions.first().expect("decision record present");
    assert_eq!(rec.decision, "received_allowed_tools");
    assert_eq!(rec.rationale, "read,grep");
}
