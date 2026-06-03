// SPDX-License-Identifier: Apache-2.0
//! End-to-end: when the model emits multiple `Task` calls in one assistant turn,
//! the agent loop runs the sub-agents CONCURRENTLY — not one-at-a-time. A worker
//! that records the peak number of simultaneously-running sub-agents must see 2.
#![allow(clippy::panic)]
#![allow(clippy::unwrap_used)]

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use origin_cas::{Store as CasStore, StoreConfig};
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_plan::{Plan, PlanStore};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage as ProvUsage};
use origin_swarm::{
    CompletionReport, Coordinator, PlanHandle, ReportStatus, Usage as SwarmUsage, WorkerContext, WorkerFn,
};
use tempfile::TempDir;
use tokio::sync::Mutex;

/// Emits TWO `Task` `tool_use` blocks on the first turn, then stops.
struct TwoTaskProvider {
    turn: AtomicU32,
}

#[async_trait]
impl Provider for TwoTaskProvider {
    fn name(&self) -> &'static str {
        "two-task"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let t = self.turn.fetch_add(1, Ordering::SeqCst);
        let blocks = if t == 0 {
            let mk = |id: &str, goal: &str| Block::ToolUse {
                id: id.into(),
                name: "Task".into(),
                input_json: serde_json::to_vec(&serde_json::json!({"goal": goal, "allowed_tools": []})).unwrap(),
                cache_marker: None,
            };
            vec![mk("t1", "alpha"), mk("t2", "beta")]
        } else {
            vec![Block::Text {
                text: "done".into(),
                cache_marker: None,
            }]
        };
        Ok(ChatResponse {
            assistant: Message {
                role: Role::Assistant,
                blocks,
            },
            usage: ProvUsage::default(),
        })
    }
}

fn open_cas(root: std::path::PathBuf) -> CasStore {
    CasStore::open(StoreConfig {
        root,
        hot_capacity: 64,
        warm_pack_target_bytes: 1 << 20,
        cold_zstd_level: 3,
    })
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_task_calls_in_one_turn_run_concurrently() {
    let tmp = TempDir::new().unwrap();
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).unwrap());
    let cas = Arc::new(open_cas(tmp.path().join("cas")));
    let plan_store = Arc::new(PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).unwrap());
    let plan = PlanHandle::new(Arc::new(Mutex::new(Plan::default())), plan_store);

    // Each sub-agent worker bumps a shared "running" counter, records the peak,
    // holds the slot, then exits. Peak == 2 only if both ran at the same time.
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
                usage: SwarmUsage::default(),
            })
        })
    });
    // Unlimited memory gate so this test isolates the agent-loop's parallel
    // dispatch from the memory-admission policy (covered in origin-swarm).
    let mut coord =
        Coordinator::new(plan, "parallel-dispatch-test").with_memory_gate(origin_swarm::AdmissionGate::unlimited_for_test());
    coord.set_default_worker(worker);

    let provider = TwoTaskProvider {
        turn: AtomicU32::new(0),
    };
    let mut session = Session::new("test-session", "scripted-model");
    let mut opts = LoopOptions::default().with_cas(Arc::clone(&cas)).without_streaming();
    opts.coordinator = Some(Arc::new(coord));

    run_loop(&mut session, "spawn two sub-agents", &provider, &AlwaysAllow, &opts)
        .await
        .expect("run_loop");

    assert_eq!(
        max_seen.load(Ordering::SeqCst),
        2,
        "both Task sub-agents must run at the same time (was serial?)"
    );
}
