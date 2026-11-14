// SPDX-License-Identifier: Apache-2.0
//! A8: a denied tool call is returned to the model as an `is_error` tool
//! result and the loop continues; only repeated denials (3 consecutive
//! all-denied turns, or 20 denied calls in total, in one run) stop the run with
//! `LoopError::Denied`, and never with an unanswered `tool_use` in the session.

#![allow(clippy::panic)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopError, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysDeny;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::collections::BTreeSet;
use std::sync::Mutex;

struct ScriptedProvider {
    responses: Mutex<Vec<ChatResponse>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl ScriptedProvider {
    /// One assistant turn per inner `Vec`, each holding that turn's blocks.
    fn new(turns: Vec<Vec<Block>>) -> Self {
        let responses = turns
            .into_iter()
            .map(|blocks| ChatResponse {
                assistant: blocks
                    .into_iter()
                    .fold(Message::new(Role::Assistant), Message::with_block),
                usage: Usage::default(),
            })
            .collect();
        Self {
            responses: Mutex::new(responses),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn take_requests(&self) -> Vec<ChatRequest> {
        std::mem::take(&mut *self.requests.lock().expect("lock"))
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

fn tool(id: &str, name: &str, args: &serde_json::Value) -> Block {
    Block::ToolUse {
        id: id.into(),
        name: name.into(),
        input_json: serde_json::to_vec(args).expect("json"),
        cache_marker: None,
    }
}

fn bash(id: &str, command: &str) -> Block {
    tool(id, "Bash", &serde_json::json!({ "command": command }))
}

/// `(tool_use_id, is_error, text)` for every tool result in `messages`.
fn tool_results(messages: &[Message]) -> Vec<(String, bool, String)> {
    messages
        .iter()
        .flat_map(|m| &m.blocks)
        .filter_map(|b| match b {
            Block::ToolResult {
                tool_use_id,
                inline,
                is_error,
                ..
            } => Some((
                tool_use_id.clone(),
                *is_error,
                String::from_utf8_lossy(inline.as_deref().unwrap_or_default()).into_owned(),
            )),
            _ => None,
        })
        .collect()
}

/// Every `tool_use` in the session has exactly one matching `tool_result`.
fn assert_every_tool_use_answered(session: &Session) {
    let uses: BTreeSet<String> = session
        .messages
        .iter()
        .flat_map(|m| &m.blocks)
        .filter_map(|b| match b {
            Block::ToolUse { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    let results: BTreeSet<String> = tool_results(&session.messages)
        .into_iter()
        .map(|(id, ..)| id)
        .collect();
    assert_eq!(uses, results, "every tool_use must have a tool_result");
}

#[tokio::test]
async fn one_denial_is_an_error_result_and_the_loop_continues() {
    let provider = ScriptedProvider::new(vec![vec![bash("tu_1", "echo hi")], vec![Block::text("done")]]);
    let mut session = Session::new("deny-once", "scripted-model");
    let opts = LoopOptions::default().without_streaming();

    let summary = run_loop(&mut session, "go", &provider, &AlwaysDeny, &opts)
        .await
        .expect("a single denial must not end the run");
    assert_eq!(summary.assistant_text, "done");

    // The second request carries the denial back to the model as is_error.
    let requests = provider.take_requests();
    assert_eq!(requests.len(), 2);
    let results = tool_results(&requests[1].messages);
    let (_, is_error, text) = results
        .iter()
        .find(|(id, ..)| id == "tu_1")
        .expect("model sees a tool result for the denied call");
    assert!(is_error, "denial must be an is_error result");
    assert!(
        text.contains("denied") && text.contains("RequiresPermission") && text.contains("user-denied"),
        "result must name the tier and rule that denied it: {text}"
    );
}

#[tokio::test]
async fn parallel_denials_in_one_turn_all_reach_the_model() {
    let provider = ScriptedProvider::new(vec![
        vec![
            bash("tu_1", "echo 1"),
            bash("tu_2", "echo 2"),
            bash("tu_3", "echo 3"),
        ],
        vec![Block::text("done")],
    ]);
    let mut session = Session::new("deny-parallel", "scripted-model");
    let opts = LoopOptions::default().without_streaming();

    let summary = run_loop(&mut session, "go", &provider, &AlwaysDeny, &opts)
        .await
        .expect("three denials in ONE turn count as one denied turn");
    assert_eq!(summary.assistant_text, "done");

    let requests = provider.take_requests();
    assert_eq!(requests.len(), 2);
    let errors: Vec<String> = tool_results(&requests[1].messages)
        .into_iter()
        .filter(|(_, is_error, _)| *is_error)
        .map(|(id, ..)| id)
        .collect();
    assert_eq!(errors, ["tu_1", "tu_2", "tu_3"]);
}

#[tokio::test]
async fn third_consecutive_denied_turn_stops_the_run() {
    let provider = ScriptedProvider::new(vec![
        vec![bash("tu_1", "echo 1")],
        vec![bash("tu_2", "echo 2"), bash("tu_3", "echo 3")],
        vec![bash("tu_4", "echo 4")],
        vec![Block::text("unreachable")],
    ]);
    let mut session = Session::new("deny-thrice", "scripted-model");
    let opts = LoopOptions::default().without_streaming();

    let err = run_loop(&mut session, "go", &provider, &AlwaysDeny, &opts)
        .await
        .expect_err("three consecutive denied turns must stop the run");
    match err {
        LoopError::Denied(name) => assert_eq!(name, "Bash"),
        other => panic!("expected Denied(Bash), got {other:?}"),
    }
    assert_eq!(provider.take_requests().len(), 3);
    assert_every_tool_use_answered(&session);
}

#[tokio::test]
async fn twentieth_total_denial_stops_the_run() {
    // Each turn: a denied Bash plus an allowed (AutoAllowed) Read, so the
    // consecutive count resets every turn and only the total cap can fire.
    // Distinct args per turn keep the stuck-loop detector out of it.
    let mut turns: Vec<Vec<Block>> = (1..=20)
        .map(|i| {
            vec![
                bash(&format!("b{i}"), &format!("echo {i}")),
                tool(
                    &format!("r{i}"),
                    "Read",
                    &serde_json::json!({ "file_path": "Cargo.toml", "offset": i, "limit": 1 }),
                ),
            ]
        })
        .collect();
    turns.push(vec![Block::text("unreachable")]);
    let provider = ScriptedProvider::new(turns);
    let mut session = Session::new("deny-total", "scripted-model");
    let opts = LoopOptions::default().without_streaming();

    let err = run_loop(&mut session, "go", &provider, &AlwaysDeny, &opts)
        .await
        .expect_err("the 20th denial must stop the run");
    assert!(
        matches!(err, LoopError::Denied(_)),
        "expected Denied, got {err:?}"
    );
    assert_eq!(provider.take_requests().len(), 20);
    assert_every_tool_use_answered(&session);
}
