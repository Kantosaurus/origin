// SPDX-License-Identifier: Apache-2.0
//! A4 (fix round 1): delegating via `Task` must never make a sub-agent more
//! privileged than its parent.
//!
//! - `Task` is permission-gated, so a parent whose prompter denies (ask mode,
//!   user says no) cannot delegate at all — no worker is spawned.
//! - A parent under a skill mask can only delegate tools the mask permits.
#![allow(clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use origin_cas::{Store as CasStore, StoreConfig};
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopError, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::{AlwaysAllow, AlwaysDeny};
use origin_plan::{Plan, PlanStore};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use origin_skills::frontmatter::SkillFrontmatter;
use origin_skills::SkillRegistry;
use origin_swarm::{CompletionReport, Coordinator, PlanHandle, ReportStatus, WorkerContext, WorkerFn};
use tempfile::TempDir;
use tokio::sync::{Mutex as AsyncMutex, RwLock};

/// Hands back queued responses in order.
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

fn reply(block: Block) -> ChatResponse {
    ChatResponse {
        assistant: Message::new(Role::Assistant).with_block(block),
        usage: Usage::default(),
    }
}

fn tool_use(name: &str, input: &serde_json::Value) -> Block {
    Block::ToolUse {
        id: format!("tu_{name}"),
        name: name.into(),
        input_json: serde_json::to_vec(input).expect("json"),
        cache_marker: None,
    }
}

const fn scripted(responses: Vec<ChatResponse>) -> ScriptedProvider {
    ScriptedProvider {
        responses: Mutex::new(responses),
    }
}

/// A coordinator whose worker is `worker`, backed by a temp plan store.
fn coordinator(tmp: &TempDir, worker: WorkerFn) -> (Arc<Coordinator>, Arc<CasStore>) {
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
    let cas = Arc::new(
        CasStore::open(StoreConfig {
            root: tmp.path().join("cas"),
            hot_capacity: 64,
            warm_pack_target_bytes: 1 << 20,
            cold_zstd_level: 3,
        })
        .expect("cas"),
    );
    let plan_store = Arc::new(PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).expect("plan store"));
    let plan = PlanHandle::new(Arc::new(AsyncMutex::new(Plan::default())), plan_store);
    let mut coord = Coordinator::new(plan, format!("origin-delegation-{}", ulid::Ulid::new()));
    coord.set_default_worker(worker);
    (Arc::new(coord), cas)
}

/// A worker that only counts how many times it was spawned.
fn counting_worker(spawned: &Arc<AtomicUsize>) -> WorkerFn {
    let spawned = Arc::clone(spawned);
    Arc::new(move |ctx: WorkerContext| {
        spawned.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(CompletionReport {
                goal: ctx.spec.goal.clone(),
                status: ReportStatus::Completed,
                plan_updates: Vec::new(),
                files_touched: Vec::new(),
                decisions: Vec::new(),
                follow_ups: Vec::new(),
                transcript_handle: [0; 32],
                usage: origin_swarm::Usage::default(),
                detail: None,
            })
        })
    })
}

fn task_call(allowed_tools: &[&str]) -> Block {
    tool_use(
        "Task",
        &serde_json::json!({"goal": "delegated", "allowed_tools": allowed_tools, "background": false}),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_is_permission_gated_by_the_parent_prompter() {
    // Denying prompter (ask mode, user says no) ⇒ Task denied, nothing spawned.
    let tmp = TempDir::new().expect("tmp");
    let spawned = Arc::new(AtomicUsize::new(0));
    let (coord, _cas) = coordinator(&tmp, counting_worker(&spawned));
    let mut opts = LoopOptions::default().without_streaming();
    opts.coordinator = Some(coord);
    let provider = scripted(vec![reply(task_call(&["Write"])), reply(Block::text("done"))]);
    let mut session = Session::new("deny", "scripted-model");
    let err = run_loop(&mut session, "go", &provider, &AlwaysDeny, &opts)
        .await
        .expect_err("a denied delegation must not run");
    match err {
        LoopError::Denied(name) => assert_eq!(name, "Task"),
        other => panic!("expected Denied(Task), got {other:?}"),
    }
    assert_eq!(
        spawned.load(Ordering::SeqCst),
        0,
        "a denied Task must not spawn a worker"
    );

    // Allowing prompter (the default mode) ⇒ delegation runs as before.
    let tmp = TempDir::new().expect("tmp");
    let spawned = Arc::new(AtomicUsize::new(0));
    let (coord, _cas) = coordinator(&tmp, counting_worker(&spawned));
    let mut opts = LoopOptions::default().without_streaming();
    opts.coordinator = Some(coord);
    let provider = scripted(vec![reply(task_call(&["Write"])), reply(Block::text("done"))]);
    let mut session = Session::new("allow", "scripted-model");
    run_loop(&mut session, "go", &provider, &AlwaysAllow, &opts)
        .await
        .expect("allowed delegation runs");
    assert_eq!(spawned.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plan_mode_parent_still_cannot_delegate() {
    // Read-only overlay still denies Task (Mutating) after the tier change, even
    // with an allowing prompter — so a plan-mode parent never spawns a worker.
    let tmp = TempDir::new().expect("tmp");
    let spawned = Arc::new(AtomicUsize::new(0));
    let (coord, _cas) = coordinator(&tmp, counting_worker(&spawned));
    let mut opts = LoopOptions::default().without_streaming();
    opts.coordinator = Some(coord);
    opts.read_only = true;
    let provider = scripted(vec![reply(task_call(&["Read"])), reply(Block::text("done"))]);
    let mut session = Session::new("plan", "scripted-model");
    let err = run_loop(&mut session, "go", &provider, &AlwaysAllow, &opts)
        .await
        .expect_err("plan mode must deny Task");
    assert!(matches!(err, LoopError::Denied(ref n) if n == "Task"), "{err:?}");
    assert_eq!(spawned.load(Ordering::SeqCst), 0);
}

fn read_task_mask() -> Arc<SkillRegistry> {
    let mut skills = SkillRegistry::new();
    skills.activate(SkillFrontmatter {
        name: "read-and-delegate".into(),
        description: "Read plus delegation".into(),
        allowed_tools: vec!["Read".into(), "Task".into()],
    });
    Arc::new(skills)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn skill_masked_parent_cannot_delegate_tools_outside_its_mask() {
    // Mask = {Read, Task}. The parent delegates [Write, Read]; the real worker's
    // model then tries Write — it must be refused (Write was narrowed away).
    let tmp = TempDir::new().expect("tmp");
    let target = tmp.path().join("escalated.txt");
    let worker_provider: Arc<dyn Provider> = Arc::new(scripted(vec![
        reply(tool_use(
            "Write",
            &serde_json::json!({"file_path": target.to_string_lossy(), "content": "pwned"}),
        )),
        reply(Block::text("worker done")),
    ]));
    let cas_for_worker = Arc::new(
        CasStore::open(StoreConfig {
            root: tmp.path().join("worker-cas"),
            hot_capacity: 64,
            warm_pack_target_bytes: 1 << 20,
            cold_zstd_level: 3,
        })
        .expect("cas"),
    );
    let worker = origin_daemon::swarm_worker::real_worker(
        Arc::new(RwLock::new(worker_provider)),
        cas_for_worker,
        origin_keyvault::KeyVault::in_memory(),
        origin_planner::Plan::default(),
        origin_daemon::config::Governance::default(),
    );
    let (coord, _cas) = coordinator(&tmp, worker);
    let mut opts = LoopOptions::default()
        .without_streaming()
        .with_skills(read_task_mask());
    opts.coordinator = Some(coord);
    let provider = scripted(vec![
        reply(task_call(&["Write", "Read"])),
        reply(Block::text("done")),
    ]);
    let mut session = Session::new("masked", "scripted-model");
    run_loop(&mut session, "go", &provider, &AlwaysAllow, &opts)
        .await
        .expect("parent loop completes");
    assert!(
        !target.exists(),
        "a skill-masked parent delegated Write (outside its mask) and the worker ran it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn masked_delegation_with_nothing_permitted_is_refused() {
    let tmp = TempDir::new().expect("tmp");
    let spawned = Arc::new(AtomicUsize::new(0));
    let (coord, _cas) = coordinator(&tmp, counting_worker(&spawned));
    let mut opts = LoopOptions::default()
        .without_streaming()
        .with_skills(read_task_mask());
    opts.coordinator = Some(coord);
    let provider = scripted(vec![reply(task_call(&["Bash"])), reply(Block::text("done"))]);
    let mut session = Session::new("masked-empty", "scripted-model");
    run_loop(&mut session, "go", &provider, &AlwaysAllow, &opts)
        .await
        .expect("parent loop completes");
    assert_eq!(spawned.load(Ordering::SeqCst), 0, "nothing permitted ⇒ no worker");
    let refused = session.messages.iter().flat_map(|m| m.blocks.iter()).any(|b| {
        matches!(b, Block::ToolResult { is_error: true, inline: Some(body), .. }
            if String::from_utf8_lossy(body).contains("skill mask"))
    });
    assert!(
        refused,
        "the Task must come back as an is_error result naming the skill mask"
    );
}

fn mask_of(tools: &[&str]) -> Arc<SkillRegistry> {
    let mut skills = SkillRegistry::new();
    skills.activate(SkillFrontmatter {
        name: "custom-mask".into(),
        description: "test mask".into(),
        allowed_tools: tools.iter().map(|t| (*t).to_string()).collect(),
    });
    Arc::new(skills)
}

/// A worker that records the `allowed_tools` it was launched with.
fn recording_worker(seen: &Arc<Mutex<Vec<Vec<String>>>>) -> WorkerFn {
    let seen = Arc::clone(seen);
    let inner = counting_worker(&Arc::new(AtomicUsize::new(0)));
    Arc::new(move |ctx: WorkerContext| {
        seen.lock().expect("lock").push(ctx.spec.allowed_tools.clone());
        inner(ctx)
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn masked_parent_cannot_delegate_inline_mcp_servers() {
    // The mask {Read, Task} gives the parent no mcp__* tool, so it must not hand a
    // worker a whole MCP server (its tools bypass `allowed_tools`).
    let tmp = TempDir::new().expect("tmp");
    let spawned = Arc::new(AtomicUsize::new(0));
    let (coord, _cas) = coordinator(&tmp, counting_worker(&spawned));
    let mut opts = LoopOptions::default()
        .without_streaming()
        .with_skills(read_task_mask());
    opts.coordinator = Some(coord);
    let call = tool_use(
        "Task",
        &serde_json::json!({"goal": "use github", "allowed_tools": ["Read"], "background": false,
                            "mcp_servers": [{"name": "github"}]}),
    );
    let provider = scripted(vec![reply(call), reply(Block::text("done"))]);
    let mut session = Session::new("masked-mcp", "scripted-model");
    run_loop(&mut session, "go", &provider, &AlwaysAllow, &opts)
        .await
        .expect("parent loop completes");
    assert_eq!(
        spawned.load(Ordering::SeqCst),
        0,
        "masked MCP delegation must not spawn"
    );
    let refused = session.messages.iter().flat_map(|m| m.blocks.iter()).any(|b| {
        matches!(b, Block::ToolResult { is_error: true, inline: Some(body), .. }
            if String::from_utf8_lossy(body).contains("skill mask"))
    });
    assert!(
        refused,
        "the Task must come back as an is_error result naming the skill mask"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn masked_parent_glob_entry_is_not_forwarded_even_if_the_mask_lists_it() {
    // The mask lists the literal string `*`; the parent matches it as a name, but
    // the worker would treat it as "every tool" — it must be dropped.
    let tmp = TempDir::new().expect("tmp");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (coord, _cas) = coordinator(&tmp, recording_worker(&seen));
    let mut opts = LoopOptions::default()
        .without_streaming()
        .with_skills(mask_of(&["*", "Read", "Task"]));
    opts.coordinator = Some(coord);
    let provider = scripted(vec![reply(task_call(&["*", "Read"])), reply(Block::text("done"))]);
    let mut session = Session::new("masked-glob", "scripted-model");
    run_loop(&mut session, "go", &provider, &AlwaysAllow, &opts)
        .await
        .expect("parent loop completes");
    let seen = seen.lock().expect("lock").clone();
    assert_eq!(
        seen,
        vec![vec!["Read".to_string()]],
        "the worker must not get `*`"
    );
}
