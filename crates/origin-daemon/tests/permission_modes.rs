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
