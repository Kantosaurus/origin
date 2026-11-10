// SPDX-License-Identifier: Apache-2.0
//! A8: a denied tool call is returned to the model as an `is_error` tool
//! result and the loop continues; only repeated denials (3 consecutive or 20
//! total in one run) stop the run with `LoopError::Denied`.

#![allow(clippy::panic)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopError, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysDeny;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::Mutex;

struct ScriptedProvider {
    responses: Mutex<Vec<ChatResponse>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl ScriptedProvider {
    fn new(blocks: Vec<Block>) -> Self {
        let responses = blocks
            .into_iter()
            .map(|b| ChatResponse {
                assistant: Message::new(Role::Assistant).with_block(b),
                usage: Usage::default(),
            })
            .collect();
        Self {
            responses: Mutex::new(responses),
            requests: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn name(&self) -> &'static str {
        "scripted"
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.requests.lock().expect("lock").push(req);
        let mut q = self.responses.lock().expect("lock");
        if q.is_empty() {
            return Err(ProviderError::Api("no more scripted responses".into()));
        }
        Ok(q.remove(0))
    }
}

fn bash(id: &str, command: &str) -> Block {
    Block::ToolUse {
        id: id.into(),
        name: "Bash".into(),
        input_json: serde_json::to_vec(&serde_json::json!({ "command": command })).expect("json"),
        cache_marker: None,
    }
}

#[tokio::test]
async fn one_denial_is_an_error_result_and_the_loop_continues() {
    let provider = ScriptedProvider::new(vec![bash("tu_1", "echo hi"), Block::text("done")]);
    let mut session = Session::new("deny-once", "scripted-model");
    let opts = LoopOptions::default().without_streaming();

    let summary = run_loop(&mut session, "go", &provider, &AlwaysDeny, &opts)
        .await
        .expect("a single denial must not end the run");
    assert_eq!(summary.assistant_text, "done");

    // The second request carries the denial back to the model as is_error.
    let requests = std::mem::take(&mut *provider.requests.lock().expect("lock"));
    assert_eq!(requests.len(), 2);
    let denial = requests[1]
        .messages
        .iter()
        .flat_map(|m| &m.blocks)
        .find_map(|b| match b {
            Block::ToolResult {
                tool_use_id,
                inline,
                is_error,
                ..
            } if tool_use_id == "tu_1" => Some((*is_error, inline.clone().unwrap_or_default())),
            _ => None,
        })
        .expect("model sees a tool result for the denied call");
    let text = String::from_utf8_lossy(&denial.1);
    assert!(denial.0, "denial must be an is_error result");
    assert!(
        text.contains("denied") && text.contains("RequiresPermission") && text.contains("user-denied"),
        "result must name the tier and rule that denied it: {text}"
    );
}

#[tokio::test]
async fn third_consecutive_denial_stops_the_run() {
    let provider = ScriptedProvider::new(vec![
        bash("tu_1", "echo 1"),
        bash("tu_2", "echo 2"),
        bash("tu_3", "echo 3"),
        Block::text("unreachable"),
    ]);
    let mut session = Session::new("deny-thrice", "scripted-model");
    let opts = LoopOptions::default().without_streaming();

    let err = run_loop(&mut session, "go", &provider, &AlwaysDeny, &opts)
        .await
        .expect_err("three consecutive denials must stop the run");
    match err {
        LoopError::Denied(name) => assert_eq!(name, "Bash"),
        other => panic!("expected Denied(Bash), got {other:?}"),
    }
    assert_eq!(provider.requests.lock().expect("lock").len(), 3);
}
