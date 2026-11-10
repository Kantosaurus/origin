// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::panic)]

use async_trait::async_trait;
use origin_cas::{Hash, Store, StoreConfig};
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

struct ScriptedProvider {
    responses: Mutex<Vec<ChatResponse>>,
}

impl ScriptedProvider {
    #[allow(clippy::missing_const_for_fn)] // Mutex::new is not const-stable
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }
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

#[tokio::test]
async fn loop_terminates_when_no_tool_use() {
    let provider = ScriptedProvider::new(vec![ChatResponse {
        assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
        usage: Usage::default(),
    }]);
    let mut session = Session::new("test", "claude-opus-4-7");
    let summary = run_loop(
        &mut session,
        "hello",
        &provider,
        &AlwaysAllow,
        &LoopOptions::default(),
    )
    .await
    .expect("loop ok");
    assert_eq!(summary.assistant_text, "done");
    assert_eq!(summary.turns, 1);
}

#[tokio::test]
async fn loop_dispatches_tool_and_continues() {
    let path = std::env::temp_dir().join(format!("origin-loop-{}.txt", ulid::Ulid::new()));
    std::fs::write(&path, "tool result content").expect("write tempfile");
    let path_str = path.to_str().expect("utf8 path").to_string();

    let tool_use = Block::ToolUse {
        id: "tu_1".into(),
        name: "Read".into(),
        input_json: serde_json::to_vec(&serde_json::json!({"file_path": path_str.clone()})).expect("json"),
        cache_marker: None,
    };
    let final_text = Block::text("final");
    let provider = ScriptedProvider::new(vec![
        ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(tool_use),
            usage: Usage::default(),
        },
        ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(final_text),
            usage: Usage::default(),
        },
    ]);

    let mut session = Session::new("test", "claude-opus-4-7");
    let summary = run_loop(
        &mut session,
        "please read the file",
        &provider,
        &AlwaysAllow,
        &LoopOptions::default().without_streaming(),
    )
    .await
    .expect("loop ok");

    let _ = std::fs::remove_file(&path);
    assert_eq!(summary.assistant_text, "final");
    assert_eq!(summary.turns, 2);
}

#[tokio::test]
async fn loop_caps_at_max_turns() {
    let path = std::env::temp_dir().join(format!("origin-cap-{}.txt", ulid::Ulid::new()));
    std::fs::write(&path, "x").expect("write tempfile");
    let path_str = path.to_str().expect("utf8 path").to_string();

    let make_tool_use = || Block::ToolUse {
        id: ulid::Ulid::new().to_string(),
        name: "Read".into(),
        input_json: serde_json::to_vec(&serde_json::json!({"file_path": path_str.clone()})).expect("json"),
        cache_marker: None,
    };
    let mut responses = Vec::new();
    for _ in 0..5 {
        responses.push(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(make_tool_use()),
            usage: Usage::default(),
        });
    }
    let provider = ScriptedProvider::new(responses);
    let mut session = Session::new("test", "claude-opus-4-7");
    let opts = LoopOptions {
        max_turns: 3,
        cas: None,
        relay_tx: None,
        streaming_disabled: true,
        ..LoopOptions::default()
    };
    let err = run_loop(&mut session, "loop", &provider, &AlwaysAllow, &opts)
        .await
        .expect_err("should hit max_turns");
    let _ = std::fs::remove_file(&path);
    let msg = format!("{err}");
    assert!(msg.contains("max_turns"), "got: {msg}");
}

#[tokio::test]
async fn tool_result_goes_to_cas_and_block_carries_handle_only() {
    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        Store::open(StoreConfig {
            root: dir.path().to_path_buf(),
            hot_capacity: 32,
            warm_pack_target_bytes: 1024 * 1024,
            cold_zstd_level: 3,
        })
        .expect("store"),
    );

    let path = std::env::temp_dir().join(format!("origin-cas-{}.txt", ulid::Ulid::new()));
    std::fs::write(&path, "tool output for cas").expect("write tempfile");
    let path_str = path.to_str().expect("utf8 path").to_string();

    let tool_use = Block::ToolUse {
        id: "tu_cas".into(),
        name: "Read".into(),
        input_json: serde_json::to_vec(&serde_json::json!({"file_path": path_str.clone()})).expect("json"),
        cache_marker: None,
    };
    let provider = ScriptedProvider::new(vec![
        ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(tool_use),
            usage: Usage::default(),
        },
        ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
            usage: Usage::default(),
        },
    ]);

    let mut session = Session::new("test", "claude-opus-4-7");
    let opts = LoopOptions {
        max_turns: 5,
        cas: Some(Arc::clone(&store)),
        relay_tx: None,
        streaming_disabled: true,
        ..LoopOptions::default()
    };

    let summary = run_loop(
        &mut session,
        "please read the file",
        &provider,
        &AlwaysAllow,
        &opts,
    )
    .await
    .expect("loop");

    let _ = std::fs::remove_file(&path);

    let tool_msg = session
        .messages
        .iter()
        .find(|m| matches!(m.role, Role::Tool))
        .expect("tool message present");
    let block = tool_msg.blocks.first().expect("at least one block");
    match block {
        Block::ToolResult {
            handle: Some(h),
            inline: None,
            ..
        } => {
            let bytes = store.get(Hash::from_bytes(*h)).expect("get").expect("present");
            assert!(!bytes.is_empty(), "tool output stored in CAS");
        }
        other => panic!("expected ToolResult with handle Some / inline None, got {other:?}"),
    }
    assert_eq!(summary.turns, 2);
}

/// Phase 11 N4.3 wiring contract: when `LoopOptions::plan` is `Some`, the
/// per-tool-result dispatch path registers every produced CAS handle into
/// the shared `Plan` with a band derived from the tool's `SideEffects`
/// metadata (`Pure` → `Sticky`, `Mutating` → `Volatile`). A clone of the
/// same `Plan` — what the Anthropic provider holds — sees the
/// registrations immediately because the inner `Arc<RwLock<…>>` is shared.
#[tokio::test]
async fn dispatch_registers_pure_tool_handle_as_sticky_in_shared_plan() {
    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        Store::open(StoreConfig {
            root: dir.path().to_path_buf(),
            hot_capacity: 32,
            warm_pack_target_bytes: 1024 * 1024,
            cold_zstd_level: 3,
        })
        .expect("store"),
    );

    let path = std::env::temp_dir().join(format!("origin-plan-wire-{}.txt", ulid::Ulid::new()));
    std::fs::write(&path, "sticky body content").expect("write tempfile");
    let path_str = path.to_str().expect("utf8 path").to_string();

    let tool_use = Block::ToolUse {
        id: "tu_plan".into(),
        name: "Read".into(), // Read has SideEffects::Pure → Band::Sticky
        input_json: serde_json::to_vec(&serde_json::json!({"file_path": path_str.clone()})).expect("json"),
        cache_marker: None,
    };
    let provider = ScriptedProvider::new(vec![
        ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(tool_use),
            usage: Usage::default(),
        },
        ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
            usage: Usage::default(),
        },
    ]);

    // Two clones of the same Plan — the daemon side (passed via LoopOptions)
    // and an "encoder side" the test holds to assert on. They share the
    // inner Arc<RwLock<HashMap>>, so registrations on either side are
    // visible on the other.
    let writer_plan = origin_planner::Plan::default();
    let encoder_plan = writer_plan.clone();
    assert_eq!(encoder_plan.handle_count(), 0, "encoder plan starts empty");

    let mut session = Session::new("test", "claude-opus-4-7");
    let opts = LoopOptions {
        max_turns: 5,
        cas: Some(Arc::clone(&store)),
        relay_tx: None,
        streaming_disabled: true,
        plan: Some(writer_plan),
        ..LoopOptions::default()
    };

    let _summary = run_loop(
        &mut session,
        "please read the file",
        &provider,
        &AlwaysAllow,
        &opts,
    )
    .await
    .expect("loop");

    let _ = std::fs::remove_file(&path);

    // The dispatch site must have registered exactly one handle (Read produced
    // one tool result this turn) into the shared map, classified as Sticky.
    assert_eq!(
        encoder_plan.handle_count(),
        1,
        "encoder plan must see the handle the dispatch site registered"
    );

    // Find the registered handle from the ToolResult block and check its band.
    let tool_msg = session
        .messages
        .iter()
        .find(|m| matches!(m.role, Role::Tool))
        .expect("tool message present");
    let handle = match tool_msg.blocks.first().expect("at least one block") {
        Block::ToolResult { handle: Some(h), .. } => *h,
        other => panic!("expected ToolResult with handle, got {other:?}"),
    };
    assert_eq!(
        encoder_plan.band_for_handle(&handle),
        Some(origin_planner::Band::Sticky),
        "Read (SideEffects::Pure) must map to Band::Sticky in the encoder's plan view"
    );
}

/// A12: a run given its own working directory (a best-of-N candidate's
/// worktree) resolves the model's relative paths there, for `Write` and for
/// `ApplyPatch`, without the process cwd ever pointing at it.
#[tokio::test]
async fn relative_tool_paths_resolve_against_loop_cwd() {
    let ws = tempdir().expect("tempdir");
    let name = format!("a12-{}.txt", ulid::Ulid::new());
    let patched = format!("a12-patch-{}.txt", ulid::Ulid::new());
    let write = Block::ToolUse {
        id: "tu_w".into(),
        name: "Write".into(),
        input_json: serde_json::to_vec(&serde_json::json!({"file_path": name, "content": "hi\n"}))
            .expect("json"),
        cache_marker: None,
    };
    let patch = format!("*** Begin Patch\n*** Add File: {patched}\n+patched\n*** End Patch\n");
    let apply = Block::ToolUse {
        id: "tu_p".into(),
        name: "ApplyPatch".into(),
        input_json: serde_json::to_vec(&serde_json::json!({ "patch": patch })).expect("json"),
        cache_marker: None,
    };
    let provider = ScriptedProvider::new(vec![
        ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(write).with_block(apply),
            usage: Usage::default(),
        },
        ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
            usage: Usage::default(),
        },
    ]);
    let opts = LoopOptions {
        cwd: Some(ws.path().to_path_buf()),
        ..LoopOptions::default().without_streaming()
    };
    let mut session = Session::new("test", "claude-opus-4-7");
    run_loop(&mut session, "write files", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");

    let here = std::env::current_dir().expect("cwd");
    let stray = [here.join(&name), here.join(&patched)];
    let leaked: Vec<_> = stray.iter().filter(|p| p.exists()).collect();
    for p in &leaked {
        let _ = std::fs::remove_file(p);
    }
    assert!(
        leaked.is_empty(),
        "written relative to the process cwd: {leaked:?}"
    );
    assert_eq!(
        std::fs::read_to_string(ws.path().join(&name)).ok().as_deref(),
        Some("hi\n")
    );
    assert_eq!(
        std::fs::read_to_string(ws.path().join(&patched)).ok().as_deref(),
        Some("patched\n")
    );
}

/// A12: with `ORIGIN_CHECKPOINTS=1`, a run with its own working directory
/// snapshots THAT tree into ITS shadow history (`<cwd>/.origin/shadow.git`),
/// not the daemon's process cwd — the user's real checkout.
#[tokio::test]
async fn checkpoints_follow_the_loop_cwd() {
    let ws = tempdir().expect("tempdir");
    let shadow = ws.path().join(".origin").join("shadow.git");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("--git-dir")
            .arg(&shadow)
            .args(args)
            .current_dir(ws.path())
            .output()
            .expect("spawn git");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    // Initialised the way `origin checkpoint` (origin-cli vcs.rs) does it.
    let ws_s = ws.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(ws.path().join(".origin")).expect("mkdir");
    git(&["--work-tree", &ws_s, "init", "-q"]);
    git(&["config", "core.worktree", &ws_s]);
    git(&["config", "user.name", "t"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "commit.gpgsign", "false"]);
    let write = Block::ToolUse {
        id: "tu_w".into(),
        name: "Write".into(),
        input_json: serde_json::to_vec(&serde_json::json!({"file_path": "snap.txt", "content": "x\n"}))
            .expect("json"),
        cache_marker: None,
    };
    let provider = ScriptedProvider::new(vec![
        ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(write),
            usage: Usage::default(),
        },
        ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
            usage: Usage::default(),
        },
    ]);
    let opts = LoopOptions {
        cwd: Some(ws.path().to_path_buf()),
        ..LoopOptions::default().without_streaming()
    };
    let mut session = Session::new("test", "claude-opus-4-7");
    std::env::set_var("ORIGIN_CHECKPOINTS", "1");
    let res = run_loop(&mut session, "write", &provider, &AlwaysAllow, &opts).await;
    std::env::remove_var("ORIGIN_CHECKPOINTS");
    res.expect("loop ok");
    let snapshot = git(&["show", "--name-only", "--format=", "HEAD"]);
    assert!(
        snapshot.lines().any(|l| l == "snap.txt"),
        "the run's own shadow history holds a snapshot of the run's tree: {snapshot:?}"
    );
}

/// A11: with test selection enabled and no `graph_rebuild` call, the code
/// graph is built in the background for the run's workspace.
#[tokio::test]
async fn test_selection_builds_the_code_graph_in_the_background() {
    let ws = tempdir().expect("tempdir");
    let src = ws.path().join("calc.py");
    std::fs::write(&src, "def add(a, b):\n    return a + b\n").expect("write");
    let db = tempdir().expect("tempdir");
    let cas = Store::open(StoreConfig {
        root: db.path().join("cas"),
        hot_capacity: 32,
        warm_pack_target_bytes: 1024 * 1024,
        cold_zstd_level: 3,
    })
    .expect("cas");
    let sql = origin_store::Store::open(db.path().join("origin.db")).expect("sql");
    let graph = Arc::new(tokio::sync::Mutex::new(
        origin_codegraph::index::CodeGraphIndex::new(cas, sql),
    ));
    let provider = ScriptedProvider::new(vec![ChatResponse {
        assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
        usage: Usage::default(),
    }]);
    let opts = LoopOptions {
        code_graph: Some(Arc::clone(&graph)),
        cwd: Some(ws.path().to_path_buf()),
        ..LoopOptions::default().without_streaming()
    };
    let mut session = Session::new("test", "claude-opus-4-7");
    std::env::set_var("ORIGIN_TEST_SELECT", "1");
    let res = run_loop(&mut session, "hi", &provider, &AlwaysAllow, &opts).await;
    std::env::remove_var("ORIGIN_TEST_SELECT");
    res.expect("loop ok");

    let file = src.display().to_string();
    for _ in 0..100 {
        if !graph.lock().await.nodes_in_file(&file).expect("query").is_empty() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("code graph never indexed {file}");
}

/// A1 regression: a fresh >2 KiB result from a Mutating tool (Bash) must reach
/// the model verbatim on the very next request. Drives the production wiring
/// end to end — real `run_loop`, real Bash, CAS, the shared `Plan` the daemon
/// gives both sides, and the real `Anthropic` wire encoder against a mock
/// `/v1/messages` — and asserts on the bytes actually sent.
#[tokio::test]
async fn large_bash_result_reaches_anthropic_verbatim_on_next_request() {
    use origin_provider_anthropic::Anthropic;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let bodies = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let cap = Arc::clone(&bodies);
    // `seq`/`1..1000` print ~4-5 KB, well over the old 2,048-byte inline budget.
    let command = if cfg!(windows) { "1..1000" } else { "seq 1 1000" };
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &wiremock::Request| {
            let body: serde_json::Value = req.body_json().expect("json");
            let after_tool = body["messages"]
                .as_array()
                .and_then(|m| m.last())
                .is_some_and(|m| m["content"][0]["type"] == "tool_result");
            cap.lock().expect("lock").push(body);
            let content = if after_tool {
                serde_json::json!([{"type": "text", "text": "done"}])
            } else {
                serde_json::json!([{"type": "tool_use", "id": "tu_bash", "name": "Bash",
                                    "input": {"command": command, "timeout": 30}}])
            };
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": content,
                "usage": {"input_tokens": 1, "output_tokens": 1,
                          "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}
            }))
        })
        .mount(&server)
        .await;

    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        Store::open(StoreConfig {
            root: dir.path().to_path_buf(),
            hot_capacity: 32,
            warm_pack_target_bytes: 1024 * 1024,
            cold_zstd_level: 3,
        })
        .expect("store"),
    );
    // Production wiring (main.rs): one Plan shared by the provider and the loop.
    let plan = origin_planner::Plan::default();
    let provider = Anthropic::with_endpoint(server.uri(), "test", "claude-opus-4-7")
        .with_cas(Arc::clone(&store))
        .with_plan(plan.clone());
    let mut session = Session::new("test", "claude-opus-4-7");
    let opts = LoopOptions {
        max_turns: 5,
        cas: Some(Arc::clone(&store)),
        relay_tx: None,
        streaming_disabled: true,
        plan: Some(plan),
        ..LoopOptions::default()
    };
    run_loop(&mut session, "print some numbers", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop");

    // The full result body the loop stored for this Bash call.
    let handle = session
        .messages
        .iter()
        .flat_map(|m| &m.blocks)
        .find_map(|b| match b {
            Block::ToolResult { handle: Some(h), .. } => Some(*h),
            _ => None,
        })
        .expect("Bash result stored as a CAS handle");
    let stored = store
        .get(Hash::from_bytes(handle))
        .expect("get")
        .expect("present");
    assert!(
        stored.len() > 2048,
        "fixture must exceed 2,048 bytes, got {}",
        stored.len()
    );

    // The first request after the tool call must carry that body verbatim.
    let next = bodies
        .lock()
        .expect("lock")
        .get(1)
        .cloned()
        .expect("a follow-up request after the Bash call");
    let sent = next["messages"]
        .as_array()
        .and_then(|m| m.last())
        .map(|m| m["content"][0]["content"].clone())
        .expect("tool_result content");
    let sent = sent.as_str().expect("content str");
    assert!(
        sent.as_bytes() == stored.as_slice(),
        "Bash result ({} bytes) must be sent verbatim; got {} bytes: {:.120}",
        stored.len(),
        sent.len(),
        sent
    );
}
