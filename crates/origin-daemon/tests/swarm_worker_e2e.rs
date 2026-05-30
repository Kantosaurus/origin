// SPDX-License-Identifier: Apache-2.0
//! End-to-end proof that the daemon's REAL swarm worker
//! ([`origin_daemon::swarm_worker::real_worker`]) actually runs a sub-agent
//! through the agent loop — not the historical noop worker.
//!
//! A `Coordinator` is built with `set_default_worker(real_worker(active))`, a
//! `Task`-style worker is spawned against a scripted provider, and the returned
//! [`CompletionReport`] must reflect a real `run_loop` execution (Completed, with
//! the scripted token usage threaded through).
#![allow(clippy::panic)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use origin_cas::{Store as CasStore, StoreConfig};
use origin_core::types::{Block, Message, Role};
use origin_plan::{Plan, PlanStore};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage as ProviderUsage};
use origin_swarm::{Budget, Coordinator, PlanHandle, ReportStatus, WorkerSpec};
use tempfile::TempDir;
use tokio::sync::{Mutex as AsyncMutex, RwLock};

/// Minimal scripted provider: hands back queued responses in order.
struct ScriptedProvider {
    responses: Mutex<Vec<ChatResponse>>,
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn name(&self) -> &'static str {
        "scripted"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let mut q = self.responses.lock().expect("lock");
        if q.is_empty() {
            return Err(ProviderError::Api("no more scripted responses".into()));
        }
        Ok(q.remove(0))
    }
}

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
async fn real_worker_runs_a_subagent_to_completion() {
    let tmp = TempDir::new().expect("tmp");
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
    let cas = Arc::new(open_cas(tmp.path().join("cas")));
    let plan_store = Arc::new(PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).expect("plan store"));
    let plan = PlanHandle::new(Arc::new(AsyncMutex::new(Plan::default())), plan_store);

    // The sub-agent's provider returns a single text response with no tool use,
    // so its run_loop terminates on turn 1 with the scripted token usage.
    let response = ChatResponse {
        assistant: Message::new(Role::Assistant).with_block(Block::text("sub-agent finished")),
        usage: ProviderUsage {
            input_tokens: 11,
            output_tokens: 7,
            ..ProviderUsage::default()
        },
    };
    let active: Arc<RwLock<Arc<dyn Provider>>> = Arc::new(RwLock::new(Arc::new(ScriptedProvider {
        responses: Mutex::new(vec![response]),
    })));

    let mut coord = Coordinator::new(plan, format!("origin-worker-e2e-{}", std::process::id()));
    coord.set_default_worker(origin_daemon::swarm_worker::real_worker(active));

    let spec = WorkerSpec {
        goal: "investigate the foo module".into(),
        allowed_tools: vec!["Read".into(), "Grep".into()],
        budget: Budget {
            max_wall_ms: 5_000,
            max_input_tokens: 10_000,
            max_output_tokens: 10_000,
            max_tool_calls: 8,
        },
        workspace: None,
        parent_actor: origin_plan::ActorId::new(0),
        model: None,
    };

    let handle = coord.spawn(spec).await.expect("spawn worker");
    let report = coord.await_completion(&handle).await.expect("await completion");

    // A real run_loop ran: Completed, with the scripted usage threaded through.
    // (The noop worker would have returned Usage::default() == zeros.)
    assert_eq!(report.status, ReportStatus::Completed);
    assert_eq!(report.goal, "investigate the foo module");
    assert_eq!(report.usage.input_tokens, 11, "scripted input tokens must flow through");
    assert_eq!(report.usage.output_tokens, 7, "scripted output tokens must flow through");
}
