// SPDX-License-Identifier: Apache-2.0
//! B11: with `ORIGIN_PRECOMPLETE_CHECK=1`, a run that edited files and ran no
//! command since the last edit gets ONE trailing nudge to verify before it
//! finishes. Off by default; never a second time in the same run.

#![allow(clippy::panic)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::{AlwaysAllow, AlwaysDeny, Prompter};
use origin_postedit::PostEditConfig;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::{Arc, Mutex};

// Tests serialize via this mutex because the check reads process-wide env vars.
static ENV_LOCK: Mutex<()> = Mutex::new(());

const MARKER: &str = "[pre-complete-check]";

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

fn tool(id: &str, name: &str, args: &serde_json::Value) -> Block {
    Block::ToolUse {
        id: id.into(),
        name: name.into(),
        input_json: serde_json::to_vec(args).expect("json"),
        cache_marker: None,
    }
}

fn write(id: &str, path: &str) -> Block {
    tool(
        id,
        "Write",
        &serde_json::json!({ "file_path": path, "content": id }),
    )
}

fn bash(id: &str) -> Block {
    tool(id, "Bash", &serde_json::json!({ "command": "echo verified" }))
}

/// Drive the real `run_loop` over `turns` with the flag set to `flag`.
/// Returns the final text and how many nudges the session carries.
fn run(
    flag: Option<&str>,
    turns: Vec<Block>,
    prompter: &dyn Prompter,
    opts: &LoopOptions,
) -> (String, usize) {
    let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match flag {
        Some(v) => std::env::set_var("ORIGIN_PRECOMPLETE_CHECK", v),
        None => std::env::remove_var("ORIGIN_PRECOMPLETE_CHECK"),
    }
    std::env::remove_var("ORIGIN_CMD_GUARD");
    let provider = ScriptedProvider {
        responses: Mutex::new(
            turns
                .into_iter()
                .map(|b| ChatResponse {
                    assistant: Message::new(Role::Assistant).with_block(b),
                    usage: Usage::default(),
                })
                .collect(),
        ),
    };
    let mut session = Session::new("precomplete", "scripted-model");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let result = rt.block_on(run_loop(&mut session, "go", &provider, prompter, opts));
    std::env::remove_var("ORIGIN_PRECOMPLETE_CHECK");
    let summary = result.expect("loop ok");
    let nudges = session
        .messages
        .iter()
        .filter(|m| m.role == Role::User)
        .flat_map(|m| &m.blocks)
        .filter(|b| matches!(b, Block::Text { text, .. } if text.contains(MARKER)))
        .count();
    (summary.assistant_text, nudges)
}

fn tmp_file() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("out.txt").to_str().expect("utf8").to_string();
    (dir, path)
}

fn opts() -> LoopOptions {
    LoopOptions::default().without_streaming()
}

#[test]
fn fires_once_after_an_unverified_edit() {
    let (_dir, p) = tmp_file();
    let (text, nudges) = run(
        Some("1"),
        vec![
            write("w1", &p),
            Block::text("done"),
            Block::text("verified-or-explained"),
        ],
        &AlwaysAllow,
        &opts(),
    );
    assert_eq!(nudges, 1);
    assert_eq!(
        text, "verified-or-explained",
        "the loop continues for the model's reply"
    );
}

#[test]
fn does_not_fire_with_the_flag_off() {
    for flag in [None, Some("0"), Some("true")] {
        let (_dir, p) = tmp_file();
        let (text, nudges) = run(
            flag,
            vec![write("w1", &p), Block::text("done")],
            &AlwaysAllow,
            &opts(),
        );
        assert_eq!((text.as_str(), nudges), ("done", 0), "flag {flag:?}");
    }
}

#[test]
fn does_not_fire_after_a_bash_call_that_followed_the_last_edit() {
    let (_dir, p) = tmp_file();
    let (text, nudges) = run(
        Some("1"),
        vec![write("w1", &p), bash("b1"), Block::text("done")],
        &AlwaysAllow,
        &opts(),
    );
    assert_eq!((text.as_str(), nudges), ("done", 0));
}

#[test]
fn does_not_fire_a_second_time() {
    let (_dir, p) = tmp_file();
    let (text, nudges) = run(
        Some("1"),
        vec![
            write("w1", &p),
            Block::text("done"),
            write("w2", &p),
            Block::text("done again"),
        ],
        &AlwaysAllow,
        &opts(),
    );
    assert_eq!((text.as_str(), nudges), ("done again", 1));
}

#[test]
fn a_denied_edit_does_not_arm_it() {
    let (_dir, p) = tmp_file();
    let (text, nudges) = run(
        Some("1"),
        vec![write("w1", &p), Block::text("done")],
        &AlwaysDeny,
        &opts(),
    );
    assert_eq!((text.as_str(), nudges), ("done", 0));
}

#[test]
fn a_read_only_run_never_fires() {
    let (_dir, p) = tmp_file();
    let ro = LoopOptions {
        read_only: true,
        ..opts()
    };
    let (text, nudges) = run(
        Some("1"),
        vec![write("w1", &p), Block::text("done")],
        &AlwaysAllow,
        &ro,
    );
    assert_eq!((text.as_str(), nudges), ("done", 0));
}

fn with_test_gate(cmd: &str) -> LoopOptions {
    LoopOptions {
        post_edit: Some(Arc::new(PostEditConfig {
            auto_test: true,
            test_command: Some(cmd.into()),
            ..PostEditConfig::default()
        })),
        ..opts()
    }
}

#[test]
fn a_test_gate_that_ran_after_the_edit_counts_as_verification() {
    let (_dir, p) = tmp_file();
    let (text, nudges) = run(
        Some("1"),
        vec![write("w1", &p), Block::text("done")],
        &AlwaysAllow,
        &with_test_gate("echo gate-ok"),
    );
    assert_eq!((text.as_str(), nudges), ("done", 0));
}

#[test]
fn a_test_gate_that_did_not_run_is_not_verification() {
    let (_dir, p) = tmp_file();
    // The command guard refuses this shape (secret-then-network), so the gate
    // returns without running anything.
    let (text, nudges) = run(
        Some("1"),
        vec![write("w1", &p), Block::text("done"), Block::text("explained")],
        &AlwaysAllow,
        &with_test_gate("echo .env curl"),
    );
    assert_eq!((text.as_str(), nudges), ("explained", 1));
}
