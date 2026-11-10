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
    let vault = origin_keyvault::KeyVault::in_memory();
    // The daemon's process-wide cache-band Plan. The worker forks a view sharing
    // its content-addressed handle bands (sub-agent prefix-cache inheritance);
    // the fork's share/isolate semantics are unit-tested in
    // `origin_planner::planner::fork_tests`. Here we just prove the plan threads
    // through `real_worker` without breaking the worker's run_loop.
    let wire_plan = origin_planner::Plan::default();
    coord.set_default_worker(origin_daemon::swarm_worker::real_worker(
        active,
        Arc::clone(&cas),
        vault,
        wire_plan,
        origin_daemon::config::Governance::default(),
    ));

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
        mcp_servers: Vec::new(),
    };

    let handle = coord.spawn(spec).await.expect("spawn worker");
    let report = coord.await_completion(&handle).await.expect("await completion");

    // A real run_loop ran: Completed, with the scripted usage threaded through.
    // (The noop worker would have returned Usage::default() == zeros.)
    assert_eq!(report.status, ReportStatus::Completed);
    assert_eq!(report.goal, "investigate the foo module");
    assert_eq!(
        report.usage.input_tokens, 11,
        "scripted input tokens must flow through"
    );
    assert_eq!(
        report.usage.output_tokens, 7,
        "scripted output tokens must flow through"
    );
}

/// A4: a worker inherits its parent's governance. The parent's
/// `governance.toml` denies `Write`; the worker's own allow-list grants `Write`
/// (the model chose it) — the deny rule must still win, so the file is never
/// written.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_inherits_parent_deny_rule() {
    let wrote = worker_write_ran("[[permission_rules]]\ntool = \"Write\"\nallow = false\n", "Write").await;
    assert!(!wrote, "the parent's deny rule must bind the worker: Write ran");
}

/// A4 fix round 1: a parent's `allow` rule must NOT widen a worker past its own
/// allow-list (rules are consulted before the worker's `AllowList` prompter).
/// The worker only has `Read`, so its `Write` must be refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parent_allow_rule_does_not_widen_worker_allow_list() {
    let wrote = worker_write_ran("[[permission_rules]]\ntool = \"Write\"\nallow = true\n", "Read").await;
    assert!(!wrote, "an allow rule let a worker without `Write` run Write");
}

/// A4 fix round 2: a worker has no coordinator, so its `Task` call (it is shown
/// the tool and told to use it) must come back as a recoverable error — not a
/// denial that ends the worker's whole run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_calling_task_recovers_and_completes() {
    let tmp = TempDir::new().expect("tmp");
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
    let cas = Arc::new(open_cas(tmp.path().join("cas")));
    let plan_store = Arc::new(PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).expect("plan store"));
    let plan = PlanHandle::new(Arc::new(AsyncMutex::new(Plan::default())), plan_store);
    let task = ChatResponse {
        assistant: Message::new(Role::Assistant).with_block(Block::ToolUse {
            id: "tu_1".into(),
            name: "Task".into(),
            input_json: serde_json::to_vec(&serde_json::json!({"goal": "nested", "allowed_tools": []}))
                .expect("json"),
            cache_marker: None,
        }),
        usage: ProviderUsage::default(),
    };
    let done = ChatResponse {
        assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
        usage: ProviderUsage::default(),
    };
    let active: Arc<RwLock<Arc<dyn Provider>>> = Arc::new(RwLock::new(Arc::new(ScriptedProvider {
        responses: Mutex::new(vec![task, done]),
    })));
    let mut coord = Coordinator::new(plan, format!("origin-worker-nested-{}", std::process::id()));
    coord.set_default_worker(origin_daemon::swarm_worker::real_worker(
        active,
        Arc::clone(&cas),
        origin_keyvault::KeyVault::in_memory(),
        origin_planner::Plan::default(),
        origin_daemon::config::Governance::default(),
    ));
    let spec = WorkerSpec {
        goal: "try to delegate".into(),
        allowed_tools: vec!["Read".into()],
        budget: Budget {
            max_wall_ms: 5_000,
            max_input_tokens: 10_000,
            max_output_tokens: 10_000,
            max_tool_calls: 4,
        },
        workspace: None,
        parent_actor: origin_plan::ActorId::new(0),
        model: None,
        mcp_servers: Vec::new(),
    };
    let handle = coord.spawn(spec).await.expect("spawn worker");
    let report = coord.await_completion(&handle).await.expect("await");
    assert_eq!(
        report.status,
        ReportStatus::Completed,
        "a nested Task must not end the worker: {:?}",
        report.detail
    );
}

/// Run a real worker under `gov_toml` with `allowed_tools = [allowed]` whose
/// scripted model issues one `Write`; returns whether the file was created.
async fn worker_write_ran(gov_toml: &str, allowed: &str) -> bool {
    let tmp = TempDir::new().expect("tmp");
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
    let cas = Arc::new(open_cas(tmp.path().join("cas")));
    let plan_store = Arc::new(PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).expect("plan store"));
    let plan = PlanHandle::new(Arc::new(AsyncMutex::new(Plan::default())), plan_store);

    let gov_path = tmp.path().join("governance.toml");
    std::fs::write(&gov_path, gov_toml).expect("gov");
    let governance = origin_daemon::config::load_governance(&gov_path).expect("load governance");

    let target = tmp.path().join("escalated.txt");
    let write = ChatResponse {
        assistant: Message::new(Role::Assistant).with_block(Block::ToolUse {
            id: "tu_1".into(),
            name: "Write".into(),
            input_json: serde_json::to_vec(
                &serde_json::json!({"file_path": target.to_string_lossy(), "content": "pwned"}),
            )
            .expect("json"),
            cache_marker: None,
        }),
        usage: ProviderUsage::default(),
    };
    let done = ChatResponse {
        assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
        usage: ProviderUsage::default(),
    };
    let active: Arc<RwLock<Arc<dyn Provider>>> = Arc::new(RwLock::new(Arc::new(ScriptedProvider {
        responses: Mutex::new(vec![write, done]),
    })));

    let mut coord = Coordinator::new(plan, format!("origin-worker-gov-{}", std::process::id()));
    coord.set_default_worker(origin_daemon::swarm_worker::real_worker(
        active,
        Arc::clone(&cas),
        origin_keyvault::KeyVault::in_memory(),
        origin_planner::Plan::default(),
        governance,
    ));

    let spec = WorkerSpec {
        goal: "write a file".into(),
        allowed_tools: vec![allowed.into()],
        budget: Budget {
            max_wall_ms: 5_000,
            max_input_tokens: 10_000,
            max_output_tokens: 10_000,
            max_tool_calls: 4,
        },
        workspace: None,
        parent_actor: origin_plan::ActorId::new(0),
        model: None,
        mcp_servers: Vec::new(),
    };
    let handle = coord.spawn(spec).await.expect("spawn worker");
    let _ = coord.await_completion(&handle).await;
    target.exists()
}

/// A5: a model-supplied inline-MCP `command` that matches no user-scope server
/// is refused with an error naming the literal command line, and is never
/// spawned (the command would create a marker file).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn model_supplied_mcp_command_is_refused_and_not_spawned() {
    let tmp = TempDir::new().expect("tmp");
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
    let cas = Arc::new(open_cas(tmp.path().join("cas")));
    let plan_store = Arc::new(PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).expect("plan store"));
    let plan = PlanHandle::new(Arc::new(AsyncMutex::new(Plan::default())), plan_store);
    let done = ChatResponse {
        assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
        usage: ProviderUsage::default(),
    };
    let active: Arc<RwLock<Arc<dyn Provider>>> = Arc::new(RwLock::new(Arc::new(ScriptedProvider {
        responses: Mutex::new(vec![done]),
    })));
    let mut coord = Coordinator::new(plan, format!("origin-worker-mcp-{}", std::process::id()));
    coord.set_default_worker(origin_daemon::swarm_worker::real_worker(
        active,
        Arc::clone(&cas),
        origin_keyvault::KeyVault::in_memory(),
        origin_planner::Plan::default(),
        origin_daemon::config::Governance::default(),
    ));

    let marker = tmp.path().join("spawned.txt");
    let (command, args) = if cfg!(windows) {
        (
            "cmd",
            vec!["/C".to_string(), format!("echo x> {}", marker.display())],
        )
    } else {
        (
            "sh",
            vec!["-c".to_string(), format!("touch {}", marker.display())],
        )
    };
    let input: origin_tools::builtins::task::TaskInput = serde_json::from_value(serde_json::json!({
        "goal": "use the evil server",
        "allowed_tools": ["mcp__evil__*"],
        "background": false,
        "mcp_servers": [{"name": "evil", "command": command, "args": args}],
    }))
    .expect("task input");

    // No user-scope servers configured ⇒ nothing the model names is admitted.
    let result = origin_tools::builtins::task::task_tool(&coord, input, &[]).await;

    assert!(
        !marker.exists(),
        "a model-supplied MCP command must never be spawned"
    );
    let err = result
        .expect_err("an unregistered MCP server must refuse the Task")
        .to_string();
    assert!(
        err.contains(&format!("{command} {}", args.join(" "))),
        "the refusal must show the literal command line: {err}"
    );
}
