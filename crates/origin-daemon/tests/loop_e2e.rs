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
        input_json: serde_json::to_vec(&serde_json::json!({"path": path_str.clone()})).expect("json"),
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
        input_json: serde_json::to_vec(&serde_json::json!({"path": path_str.clone()})).expect("json"),
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
        input_json: serde_json::to_vec(&serde_json::json!({"path": path_str.clone()})).expect("json"),
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
        input_json: serde_json::to_vec(&serde_json::json!({"path": path_str.clone()})).expect("json"),
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
