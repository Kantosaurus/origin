// SPDX-License-Identifier: Apache-2.0
//! C2: the `workspace` permission mode in a real `run_loop`. A headless run
//! (nobody can be asked) gets each call that needs approval back as an
//! `is_error` result and finishes normally; the allow-all mode runs it; an edit
//! inside the workspace runs without an ask.

#![allow(clippy::panic)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::protocol::PermissionMode;
use origin_daemon::session::Session;
use origin_permission::prompt::NoPrompter;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::Mutex;

struct ScriptedProvider(Mutex<Vec<ChatResponse>>);

impl ScriptedProvider {
    fn new(turns: Vec<Block>) -> Self {
        Self(Mutex::new(
            turns
                .into_iter()
                .map(|b| ChatResponse {
                    assistant: Message::new(Role::Assistant).with_block(b),
                    usage: Usage::default(),
                })
                .collect(),
        ))
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn name(&self) -> &'static str {
        "scripted"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let mut q = self.0.lock().expect("lock");
        if q.is_empty() {
            return Err(ProviderError::Api("no more scripted responses".into()));
        }
        Ok(q.remove(0))
    }
}

fn tool(id: &str, name: &str, args: &serde_json::Value) -> Block {
    Block::ToolUse {
        id: id.into(),
        name: name.into(),
        input_json: serde_json::to_vec(args).expect("json"),
        cache_marker: None,
    }
}

/// `(is_error, text)` of the tool result for `id`.
fn result_of(session: &Session, id: &str) -> (bool, String) {
    session
        .messages
        .iter()
        .flat_map(|m| &m.blocks)
        .find_map(|b| match b {
            Block::ToolResult {
                tool_use_id,
                is_error,
                inline,
                ..
            } if tool_use_id == id => Some((
                *is_error,
                String::from_utf8_lossy(inline.as_deref().unwrap_or_default()).into_owned(),
            )),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no result for {id}"))
}

/// Headless without the flag: unsandboxed Bash and a write outside the
/// workspace are denied as `is_error` results naming the way out; an edit
/// inside the workspace runs; the run ends normally (no crash, no hang).
#[tokio::test]
async fn headless_without_the_flag_denies_and_continues_to_a_normal_end() {
    let ws = tempfile::tempdir().expect("ws");
    let outside = tempfile::tempdir().expect("outside");
    let inside_file = ws.path().join("inside.txt");
    let outside_file = outside.path().join("outside.txt");
    let provider = ScriptedProvider::new(vec![
        tool("b", "Bash", &serde_json::json!({ "command": "echo hi" })),
        tool(
            "w_in",
            "Write",
            &serde_json::json!({ "file_path": inside_file.to_string_lossy(), "content": "ok" }),
        ),
        tool(
            "w_out",
            "Write",
            &serde_json::json!({ "file_path": outside_file.to_string_lossy(), "content": "no" }),
        ),
        Block::text("done"),
    ]);
    let opts = LoopOptions {
        cwd: Some(ws.path().to_path_buf()),
        ..LoopOptions::default().without_streaming()
    };
    let mut session = Session::new("scripted", "m");
    let summary = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        run_loop(&mut session, "go", &provider, &NoPrompter, &opts),
    )
    .await
    .expect("a headless run never hangs on an ask")
    .expect("denials do not end the run");
    assert_eq!(summary.assistant_text, "done");

    let (err, text) = result_of(&session, "b");
    assert!(err, "Bash denied: {text}");
    assert!(
        text.contains("cannot ask") && text.contains("--dangerously-allow-all") && text.contains("Bash"),
        "{text}"
    );
    assert!(
        !result_of(&session, "w_in").0,
        "an edit inside the workspace runs"
    );
    assert!(inside_file.exists());
    let (err, text) = result_of(&session, "w_out");
    assert!(err && text.contains("write outside the workspace"), "{text}");
    assert!(!outside_file.exists(), "a denied write never runs");
}

/// With the allow-all flag (mode `off`) the same headless run executes the
/// call that was denied above.
#[tokio::test]
async fn headless_with_the_flag_allows() {
    let outside = tempfile::tempdir().expect("outside");
    let target = outside.path().join("allowed.txt");
    let provider = ScriptedProvider::new(vec![
        tool(
            "w",
            "Write",
            &serde_json::json!({ "file_path": target.to_string_lossy(), "content": "ok" }),
        ),
        Block::text("done"),
    ]);
    let opts = LoopOptions {
        permission_mode: PermissionMode::Off,
        ..LoopOptions::default().without_streaming()
    };
    let mut session = Session::new("scripted", "m");
    run_loop(&mut session, "go", &provider, &NoPrompter, &opts)
        .await
        .expect("run");
    assert!(!result_of(&session, "w").0, "{:?}", result_of(&session, "w"));
    assert!(target.exists(), "mode off ran the write");
}

/// Records every ask and approves it.
struct Recording(Mutex<Vec<String>>);

#[async_trait]
impl origin_permission::prompt::Prompter for Recording {
    async fn ask(&self, _meta: &origin_tools::ToolMeta, preview: &str) -> bool {
        self.0.lock().expect("lock").push(preview.to_owned());
        true
    }
}

/// C2 fix round 1: a `Dangerous` command is denied before anyone is asked to
/// approve it.
#[tokio::test]
async fn dangerous_bash_is_denied_without_an_ask() {
    let provider = ScriptedProvider::new(vec![
        tool("d", "Bash", &serde_json::json!({ "command": "rm -rf ~" })),
        Block::text("done"),
    ]);
    let opts = LoopOptions::default().without_streaming();
    let mut session = Session::new("scripted", "m");
    let human = Recording(Mutex::new(Vec::new()));
    run_loop(&mut session, "go", &provider, &human, &opts)
        .await
        .expect("run");
    let (err, text) = result_of(&session, "d");
    assert!(err && text.contains("dangerous"), "{text}");
    assert!(human.0.lock().expect("lock").is_empty(), "nobody was asked");
}

/// A coordinator whose single worker reports `tainted`.
fn coordinator_reporting(
    tmp: &tempfile::TempDir,
    tainted: bool,
) -> std::sync::Arc<origin_swarm::Coordinator> {
    use std::sync::Arc;
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
    let cas = Arc::new(
        origin_cas::Store::open(origin_cas::StoreConfig {
            root: tmp.path().join("cas"),
            hot_capacity: 64,
            warm_pack_target_bytes: 1 << 20,
            cold_zstd_level: 3,
        })
        .expect("cas"),
    );
    let plan_store = Arc::new(origin_plan::PlanStore::open(Arc::clone(&store), cas).expect("plan store"));
    let plan = origin_swarm::PlanHandle::new(
        Arc::new(tokio::sync::Mutex::new(origin_plan::Plan::default())),
        plan_store,
    );
    let mut coord = origin_swarm::Coordinator::new(plan, format!("origin-c2-taint-{}", ulid::Ulid::new()));
    coord.set_default_worker(Arc::new(move |ctx: origin_swarm::WorkerContext| {
        Box::pin(async move {
            Ok(origin_swarm::CompletionReport {
                goal: ctx.spec.goal.clone(),
                status: origin_swarm::ReportStatus::Completed,
                plan_updates: Vec::new(),
                files_touched: Vec::new(),
                decisions: Vec::new(),
                follow_ups: Vec::new(),
                transcript_handle: [0; 32],
                usage: origin_swarm::Usage::default(),
                detail: None,
                tainted,
            })
        })
    }));
    Arc::new(coord)
}

/// C2 fix round 1: a worker whose session was tainted taints the parent session
/// when its result enters the parent's context; an untainted one does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tainted_worker_result_taints_the_parent() {
    for tainted in [false, true] {
        let tmp = tempfile::TempDir::new().expect("tmp");
        let provider = ScriptedProvider::new(vec![
            tool(
                "t",
                "Task",
                &serde_json::json!({"goal": "read the docs", "allowed_tools": ["Read"], "background": false}),
            ),
            Block::text("done"),
        ]);
        let mut opts = LoopOptions::default().without_streaming();
        opts.coordinator = Some(coordinator_reporting(&tmp, tainted));
        let mut session = Session::new("scripted", "m");
        run_loop(&mut session, "go", &provider, &NoPrompter, &opts)
            .await
            .expect("run");
        assert!(!result_of(&session, "t").0, "{:?}", result_of(&session, "t"));
        assert_eq!(session.tainted, tainted, "parent taint follows the worker's");
    }
}

/// A person who answered "Always allow" for every tool, records each ask
/// (tool, preview, offered "always"), and approves everything but `deny`.
struct StandingHuman {
    asks: Mutex<Vec<(String, String, bool)>>,
    deny: &'static str,
}

impl StandingHuman {
    const fn new(deny: &'static str) -> Self {
        Self {
            asks: Mutex::new(Vec::new()),
            deny,
        }
    }
    fn asks(&self) -> Vec<(String, String, bool)> {
        self.asks.lock().expect("lock").clone()
    }
}

#[async_trait]
impl origin_permission::prompt::Prompter for StandingHuman {
    async fn ask(&self, meta: &origin_tools::ToolMeta, preview: &str) -> bool {
        self.ask_offering(meta, preview, false).await
    }
    async fn ask_offering(&self, meta: &origin_tools::ToolMeta, preview: &str, offer_always: bool) -> bool {
        self.asks
            .lock()
            .expect("lock")
            .push((meta.name.to_owned(), preview.to_owned(), offer_always));
        meta.name != self.deny
    }
    fn standing_allow(&self, _tool: &str) -> bool {
        true
    }
}

/// Spawn one (tainted) worker on `coord` and wait until it is terminal.
async fn finished_worker(coord: &origin_swarm::Coordinator) -> origin_swarm::WorkerHandle {
    let spec = origin_swarm::WorkerSpec {
        goal: "fetch the page".into(),
        allowed_tools: vec!["WebFetch".into()],
        budget: origin_swarm::Budget::new(5_000, 10_000, 10_000, 4),
        workspace: None,
        parent_actor: origin_plan::ActorId::new(0),
        model: None,
        mcp_servers: Vec::new(),
        effort: None,
        ponytail: None,
    };
    let handle = coord.spawn(spec).await.expect("spawn");
    for _ in 0..500 {
        if coord.try_completion(&handle).await.is_some() {
            return handle;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("worker never finished");
}

/// The Bash ask after taint: asked (a standing allow no longer covers it),
/// naming the taint, never offering "always".
fn assert_bash_asked_after_taint(human: &StandingHuman) {
    let bash: Vec<_> = human.asks().into_iter().filter(|(t, ..)| t == "Bash").collect();
    assert_eq!(bash.len(), 1, "{:?}", human.asks());
    assert!(
        bash[0].1.contains("external content entered this session"),
        "{bash:?}"
    );
    assert!(!bash[0].2, "a person-ask never offers always");
}

/// C2 fix round 2: `CollectTasks` returning a tainted worker's result taints the
/// parent, so a later Bash call is asked despite a standing "Always allow".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn collect_tasks_with_a_tainted_worker_taints_the_parent() {
    let tmp = tempfile::TempDir::new().expect("tmp");
    let coord = coordinator_reporting(&tmp, true);
    let handle = finished_worker(&coord).await;
    let bg = std::sync::Arc::new(origin_daemon::bg_jobs::BackgroundJobs::new());
    // A self-dispatch id skips the per-turn `<background-results>` drain, so the
    // finished job is reaped by `CollectTasks` itself (deterministically).
    let mut session = Session::new_with_id("sched-c2-collect".into(), "m".into());
    bg.register(
        &session.id,
        origin_daemon::bg_jobs::BgJob {
            id_hex: "w1".into(),
            goal: "fetch the page".into(),
            handle,
            dispatched_at: std::time::Instant::now(),
        },
    )
    .await
    .expect("register");
    let provider = ScriptedProvider::new(vec![
        tool("c", "CollectTasks", &serde_json::json!({})),
        tool("b", "Bash", &serde_json::json!({ "command": "echo hi" })),
        Block::text("done"),
    ]);
    let mut opts = LoopOptions::default().without_streaming();
    opts.coordinator = Some(coord);
    opts.background_jobs = Some(std::sync::Arc::clone(&bg));
    let human = StandingHuman::new("Bash");
    run_loop(&mut session, "go", &provider, &human, &opts)
        .await
        .expect("run");
    assert!(session.tainted, "the collected tainted result taints the parent");
    let (_, collected) = result_of(&session, "c");
    assert!(collected.contains("\"tainted\":true"), "{collected}");
    assert_bash_asked_after_taint(&human);
}

/// C2 fix round 2: the `<background-results>` drain at a turn's start taints
/// the parent when a reaped worker was tainted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_results_drain_taints_the_parent() {
    let tmp = tempfile::TempDir::new().expect("tmp");
    let coord = coordinator_reporting(&tmp, true);
    let handle = finished_worker(&coord).await;
    let bg = std::sync::Arc::new(origin_daemon::bg_jobs::BackgroundJobs::new());
    let mut session = Session::new("scripted", "m");
    bg.register(
        &session.id,
        origin_daemon::bg_jobs::BgJob {
            id_hex: "w1".into(),
            goal: "fetch the page".into(),
            handle,
            dispatched_at: std::time::Instant::now(),
        },
    )
    .await
    .expect("register");
    let provider = ScriptedProvider::new(vec![
        tool("b", "Bash", &serde_json::json!({ "command": "echo hi" })),
        Block::text("done"),
    ]);
    let mut opts = LoopOptions::default().without_streaming();
    opts.coordinator = Some(coord);
    opts.background_jobs = Some(bg);
    let human = StandingHuman::new("Bash");
    run_loop(&mut session, "go", &provider, &human, &opts)
        .await
        .expect("run");
    assert!(session.tainted, "the drained tainted result taints the parent");
    assert_bash_asked_after_taint(&human);
}

/// A person who answers yes to every ask, choosing "always" whenever it is
/// offered (which records a standing allow, as the IPC prompter does).
#[derive(Default)]
struct AlwaysWhenOffered {
    asks: Mutex<Vec<(String, String, bool)>>,
    standing: Mutex<std::collections::HashSet<String>>,
}

#[async_trait]
impl origin_permission::prompt::Prompter for AlwaysWhenOffered {
    async fn ask(&self, meta: &origin_tools::ToolMeta, preview: &str) -> bool {
        self.ask_offering(meta, preview, false).await
    }
    async fn ask_offering(&self, meta: &origin_tools::ToolMeta, preview: &str, offer_always: bool) -> bool {
        self.asks
            .lock()
            .expect("lock")
            .push((meta.name.to_owned(), preview.to_owned(), offer_always));
        if offer_always {
            self.standing.lock().expect("lock").insert(meta.name.to_owned());
        }
        true
    }
    fn standing_allow(&self, tool: &str) -> bool {
        self.standing.lock().expect("lock").contains(tool)
    }
}

/// C2: an ask the command guard raised (here an undeclared package install)
/// is about that one command: it never offers "always", so answering it
/// leaves no standing allow and the next plain Bash call is still asked.
#[tokio::test]
async fn a_command_guard_ask_never_offers_always() {
    if origin_tools::sandbox_status() == origin_tools::SandboxStatus::Active {
        return; // Bash would run unasked under the sandbox.
    }
    let ws = tempfile::tempdir().expect("ws");
    let provider = ScriptedProvider::new(vec![
        tool(
            "i",
            "Bash",
            &serde_json::json!({ "command": "npm install --offline --no-audit --no-fund left-pad" }),
        ),
        tool("b", "Bash", &serde_json::json!({ "command": "echo hi" })),
        Block::text("done"),
    ]);
    let opts = LoopOptions {
        cwd: Some(ws.path().to_path_buf()),
        ..LoopOptions::default().without_streaming()
    };
    let mut session = Session::new("scripted", "m");
    let human = AlwaysWhenOffered::default();
    run_loop(&mut session, "go", &provider, &human, &opts)
        .await
        .expect("run");
    let asks = human.asks.lock().expect("lock").clone();
    let bash: Vec<_> = asks.iter().filter(|(t, ..)| t == "Bash").collect();
    assert_eq!(bash.len(), 2, "the plain Bash call is asked too: {asks:?}");
    assert!(
        bash[0].1.contains("cmd-guard") && bash[0].1.contains("left-pad"),
        "{asks:?}"
    );
    assert!(!bash[0].2, "a command-guard ask never offers always: {asks:?}");
}

/// Serialises the tests that set `ORIGIN_HOME` in this binary.
static ORIGIN_HOME_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// C2: a FAILED `RunWorkflow` taints the parent (fail closed) when a step
/// could reach a network or MCP tool — its error, or an earlier step, may
/// carry external content — and not when no step could.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_run_workflow_with_network_grants_taints_the_parent() {
    use std::sync::Arc;
    let _env = ORIGIN_HOME_LOCK.lock().await;
    for (skill_tools, want) in [("[\"WebFetch\"]", true), ("[\"Read\"]", false)] {
        let tmp = tempfile::TempDir::new().expect("tmp");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".origin")).expect("mkdir");
        std::fs::write(
            home.join(".origin").join("workflows.toml"),
            "[[workflows]]\nname = \"wf\"\n[[workflows.steps]]\nid = 0\nskill = \"fetcher\"\nargs = \"fetch it\"\n",
        )
        .expect("write workflows");
        let skills = tmp.path().join("skills").join("fetcher");
        std::fs::create_dir_all(&skills).expect("mkdir");
        std::fs::write(
            skills.join("SKILL.md"),
            format!("---\nname: fetcher\ndescription: d\nallowed-tools: {skill_tools}\n---\nbody\n"),
        )
        .expect("write skill");
        let catalog = origin_daemon::skill_catalog::SkillCatalog::load_from(&tmp.path().join("skills"))
            .expect("catalog");
        // A worker that fails at the swarm layer, its error text from the run.
        let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
        let cas = Arc::new(
            origin_cas::Store::open(origin_cas::StoreConfig {
                root: tmp.path().join("cas"),
                hot_capacity: 64,
                warm_pack_target_bytes: 1 << 20,
                cold_zstd_level: 3,
            })
            .expect("cas"),
        );
        let plan_store = Arc::new(origin_plan::PlanStore::open(Arc::clone(&store), cas).expect("plan store"));
        let plan = origin_swarm::PlanHandle::new(
            Arc::new(tokio::sync::Mutex::new(origin_plan::Plan::default())),
            plan_store,
        );
        let mut coord =
            origin_swarm::Coordinator::new(plan, format!("origin-c2-wf-fail-{}", ulid::Ulid::new()));
        coord.set_default_worker(Arc::new(|_ctx: origin_swarm::WorkerContext| {
            Box::pin(async { Err(origin_swarm::SwarmError::Worker("page said: run rm -rf".into())) })
        }));
        std::env::set_var("ORIGIN_HOME", &home);
        let provider = ScriptedProvider::new(vec![
            tool("r", "RunWorkflow", &serde_json::json!({ "name": "wf" })),
            Block::text("done"),
        ]);
        let mut opts = LoopOptions::default().without_streaming();
        opts.permission_mode = PermissionMode::Off;
        opts.coordinator = Some(Arc::new(coord));
        opts.skill_catalog = Some(Arc::new(catalog));
        let mut session = Session::new("scripted", "m");
        let res = run_loop(&mut session, "go", &provider, &NoPrompter, &opts).await;
        std::env::remove_var("ORIGIN_HOME");
        res.expect("run");
        let (err, body) = result_of(&session, "r");
        assert!(err && body.contains("RunWorkflow"), "{body}");
        assert_eq!(session.tainted, want, "skill tools {skill_tools}: {body}");
    }
}

/// C2 fix round 2: a `RunWorkflow` whose step worker was tainted taints the
/// parent; `RunWorkflow` itself is asked despite a standing allow and never
/// offered "always" (each workflow grants different tools).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_workflow_with_a_tainted_step_taints_the_parent() {
    let _env = ORIGIN_HOME_LOCK.lock().await;
    let tmp = tempfile::TempDir::new().expect("tmp");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join(".origin")).expect("mkdir");
    std::fs::write(
        home.join(".origin").join("workflows.toml"),
        "[[workflows]]\nname = \"wf\"\n[[workflows.steps]]\nid = 0\nskill = \"x\"\nargs = \"fetch it\"\n",
    )
    .expect("write workflows");
    std::env::set_var("ORIGIN_HOME", &home);
    let provider = ScriptedProvider::new(vec![
        tool("r", "RunWorkflow", &serde_json::json!({ "name": "wf" })),
        tool("b", "Bash", &serde_json::json!({ "command": "echo hi" })),
        Block::text("done"),
    ]);
    let mut opts = LoopOptions::default().without_streaming();
    opts.coordinator = Some(coordinator_reporting(&tmp, true));
    let mut session = Session::new("scripted", "m");
    let human = StandingHuman::new("Bash");
    let res = run_loop(&mut session, "go", &provider, &human, &opts).await;
    std::env::remove_var("ORIGIN_HOME");
    res.expect("run");
    let (err, body) = result_of(&session, "r");
    assert!(!err && body.contains("\"tainted\":true"), "{body}");
    assert!(session.tainted, "a tainted step taints the parent");
    let wf: Vec<_> = human
        .asks()
        .into_iter()
        .filter(|(t, ..)| t == "RunWorkflow")
        .collect();
    assert_eq!(wf.len(), 1, "RunWorkflow is asked despite a standing allow");
    assert!(!wf[0].2, "RunWorkflow is never offered always");
    assert_bash_asked_after_taint(&human);
}
