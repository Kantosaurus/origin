// SPDX-License-Identifier: Apache-2.0
//! End-to-end for `workflow_runner::run_workflow`: an authored workflow must
//! fan out to real swarm workers as a phase-layered parallel DAG.
//!
//! Two assertions, both via a recording mock worker installed with
//! `Coordinator::set_default_worker` (the pattern from
//! `swarm_parallel_dispatch.rs`):
//!   (a) steps run in dependency-layer ORDER — a step never starts before its
//!       dependencies have finished;
//!   (b) two INDEPENDENT same-layer steps are dispatched TOGETHER (run
//!       concurrently) before the next layer begins.
#![allow(clippy::panic)]
#![allow(clippy::unwrap_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use origin_daemon::skill_catalog::SkillCatalog;
use origin_daemon::workflow_runner::run_workflow;
use origin_daemon::workflows::{Workflow, WorkflowStep};
use origin_plan::{Plan, PlanStore};
use origin_swarm::{
    CompletionReport, Coordinator, PlanHandle, ReportStatus, Usage as SwarmUsage, WorkerContext, WorkerFn,
};
use tempfile::TempDir;
use tokio::sync::Mutex;

/// Shared recorder: tracks concurrent worker count + peak, and the order goals
/// started/finished, so the test can assert both layering and parallelism.
#[derive(Default)]
struct Recorder {
    current: AtomicUsize,
    peak: AtomicUsize,
    started_order: StdMutex<Vec<String>>,
    finished_order: StdMutex<Vec<String>>,
}

fn step(id: usize, skill: &str, args: &str, deps: &[usize]) -> WorkflowStep {
    WorkflowStep {
        id,
        skill: skill.into(),
        args: if args.is_empty() {
            None
        } else {
            Some(args.into())
        },
        depends_on: deps.to_vec(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workflow_fans_out_per_layer_in_dependency_order() {
    let tmp = TempDir::new().unwrap();
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).unwrap());
    let cas = Arc::new(
        origin_cas::Store::open(origin_cas::StoreConfig {
            root: tmp.path().join("cas"),
            hot_capacity: 64,
            warm_pack_target_bytes: 1 << 20,
            cold_zstd_level: 3,
        })
        .unwrap(),
    );
    let plan_store = Arc::new(PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).unwrap());
    let plan = PlanHandle::new(Arc::new(Mutex::new(Plan::default())), plan_store);

    let rec = Arc::new(Recorder::default());
    let rec2 = Arc::clone(&rec);
    let worker: WorkerFn = Arc::new(move |ctx: WorkerContext| {
        let rec = Arc::clone(&rec2);
        Box::pin(async move {
            let goal = ctx.spec.goal.clone();
            rec.started_order.lock().unwrap().push(goal.clone());
            let now = rec.current.fetch_add(1, Ordering::SeqCst) + 1;
            rec.peak.fetch_max(now, Ordering::SeqCst);
            // Hold the slot so a genuinely-parallel layer overlaps in time.
            tokio::time::sleep(Duration::from_millis(80)).await;
            rec.current.fetch_sub(1, Ordering::SeqCst);
            rec.finished_order.lock().unwrap().push(goal.clone());
            Ok(CompletionReport {
                goal,
                status: ReportStatus::Completed,
                plan_updates: Vec::new(),
                files_touched: Vec::new(),
                decisions: Vec::new(),
                follow_ups: Vec::new(),
                transcript_handle: [0; 32],
                usage: SwarmUsage::default(),
            })
        })
    });

    let mut coord = Coordinator::new(plan, "workflow-runner-test")
        .with_memory_gate(origin_swarm::AdmissionGate::unlimited_for_test());
    coord.set_default_worker(worker);

    // Empty catalog: every step skill is absent, so the runner uses each step's
    // `args` as the worker goal and the default tool set. Goals are distinct so
    // we can track order.
    let catalog = SkillCatalog::default();

    // Diamond DAG: explore (0) -> {plan-a (1), plan-b (2)} -> verify (3).
    let wf = Workflow {
        name: "diamond".into(),
        description: None,
        steps: vec![
            step(0, "explore-skill", "explore", &[]),
            step(1, "plan-a-skill", "plan-a", &[0]),
            step(2, "plan-b-skill", "plan-b", &[0]),
            step(3, "verify-skill", "verify", &[1, 2]),
        ],
    };

    let report = run_workflow(&wf, &coord, &catalog).await.expect("run_workflow");

    // (b) The middle layer's two independent steps ran at the same time.
    assert_eq!(
        rec.peak.load(Ordering::SeqCst),
        2,
        "the two same-layer steps must run concurrently (was the layer serialized?)"
    );

    // (a) Dependency order: `explore` finished before either `plan-*` started,
    // and both `plan-*` finished before `verify` started.
    let started = rec.started_order.lock().unwrap().clone();
    let finished = rec.finished_order.lock().unwrap().clone();
    let started_pos = |g: &str| started.iter().position(|x| x == g).unwrap();
    let finished_pos = |g: &str| finished.iter().position(|x| x == g).unwrap();

    assert!(
        finished_pos("explore") < started_pos("plan-a") && finished_pos("explore") < started_pos("plan-b"),
        "explore must finish before either plan step starts; started={started:?} finished={finished:?}"
    );
    assert!(
        finished_pos("plan-a") < started_pos("verify") && finished_pos("plan-b") < started_pos("verify"),
        "both plan steps must finish before verify starts; started={started:?} finished={finished:?}"
    );

    // The aggregate report reflects the layering and per-step completion.
    assert_eq!(report.name, "diamond");
    assert_eq!(report.layers, 3);
    assert_eq!(report.steps.len(), 4);
    assert!(report.steps.iter().all(|s| s.status == "completed"));
    // verify ran in the last layer.
    let verify = report.steps.iter().find(|s| s.skill == "verify-skill").unwrap();
    assert_eq!(verify.layer, 2);
}
