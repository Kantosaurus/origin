use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::Mutex;

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
        LoopOptions::default(),
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
        LoopOptions::default(),
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
    let opts = LoopOptions { max_turns: 3 };
    let err = run_loop(&mut session, "loop", &provider, &AlwaysAllow, opts)
        .await
        .expect_err("should hit max_turns");
    let _ = std::fs::remove_file(&path);
    let msg = format!("{err}");
    assert!(msg.contains("max_turns"), "got: {msg}");
}
