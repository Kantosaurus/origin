// SPDX-License-Identifier: Apache-2.0
//! C5: with the repro gate on, the model's reproduction test is run before its
//! first source edit lands and must really fail. A test that passes, or never
//! runs, is rejected (bounded by the repair budget); a confirmed failure is
//! handed to the model as the diagnosis. Also: a RED gate on the last allowed
//! turn returns the answer instead of `MaxTurns`.

#![allow(clippy::panic)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopError, LoopOptions, LoopSummary};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_postedit::PostEditConfig;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::{Arc, Mutex};

// The loop reads process-wide env vars (`ORIGIN_CMD_GUARD`, ...).
static ENV_LOCK: Mutex<()> = Mutex::new(());

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

fn write(id: &str, path: &str, content: &str) -> Block {
    Block::ToolUse {
        id: id.into(),
        name: "Write".into(),
        // `force`: the seeded files were never Read in the session.
        input_json: serde_json::to_vec(
            &serde_json::json!({ "file_path": path, "content": content, "force": true }),
        )
        .expect("json"),
        cache_marker: None,
    }
}

/// A pytest stand-in: exit 2 (collection error) when the reproduction test
/// says `collect-error`, exit 1 (a failing test) when it says `fails` and the
/// source is not `fixed` yet, else exit 0.
#[cfg(unix)]
const FAKE_PYTEST: &str = "if grep -q collect-error tests/test_bug.py; then echo 'ERROR collecting pytest'; \
     exit 2; fi; if grep -q fails tests/test_bug.py; then if grep -q fixed src.py; then echo 'passed pytest'; \
     else echo 'FAILED test_bug pytest'; exit 1; fi; fi; echo 'passed pytest'";
#[cfg(windows)]
const FAKE_PYTEST: &str =
    "if (Select-String -Quiet collect-error tests/test_bug.py) { echo 'ERROR collecting \
     pytest'; exit 2 }; if ((Select-String -Quiet fails tests/test_bug.py) -and -not (Select-String -Quiet \
     fixed src.py)) { echo 'FAILED test_bug pytest'; exit 1 }; echo 'passed pytest'";

struct Run {
    result: Result<LoopSummary, LoopError>,
    /// `(is_error, body)` of every tool result, in order.
    tool_results: Vec<(bool, String)>,
    src: String,
}

fn run(cfg: PostEditConfig, max_turns: u32, turns: Vec<Block>) -> Run {
    let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    std::env::remove_var("ORIGIN_CMD_GUARD");
    std::env::remove_var("ORIGIN_TEST_SELECT");
    std::env::remove_var("ORIGIN_PRECOMPLETE_CHECK");
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("tests")).expect("mkdir");
    std::fs::write(dir.path().join("src.py"), "buggy\n").expect("seed");
    let opts = LoopOptions {
        max_turns,
        cwd: Some(dir.path().to_path_buf()),
        post_edit: Some(Arc::new(cfg)),
        ..LoopOptions::default().without_streaming()
    };
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
    let mut session = Session::new("repro", "scripted-model");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let result = rt.block_on(run_loop(
        &mut session,
        "fix the bug",
        &provider,
        &AlwaysAllow,
        &opts,
    ));
    let tool_results = session
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .flat_map(|m| &m.blocks)
        .filter_map(|b| match b {
            Block::ToolResult {
                inline: Some(v),
                is_error,
                ..
            } => Some((*is_error, String::from_utf8_lossy(v).into_owned())),
            _ => None,
        })
        .collect();
    let src = std::fs::read_to_string(dir.path().join("src.py")).expect("src");
    Run {
        result,
        tool_results,
        src,
    }
}

fn repro_gate(cmd: &str, budget: u32) -> PostEditConfig {
    PostEditConfig {
        repro_gate: true,
        test_command: Some(cmd.into()),
        max_repair_iters: budget,
        ..PostEditConfig::default()
    }
}

#[test]
fn a_confirmed_failure_reaches_the_model_as_the_diagnosis() {
    let r = run(
        repro_gate(FAKE_PYTEST, 2),
        10,
        vec![
            write("t1", "tests/test_bug.py", "fails"),
            write("s1", "src.py", "fixed"),
            Block::text("done"),
        ],
    );
    assert_eq!(r.result.expect("loop ok").assistant_text, "done");
    assert_eq!(r.src, "fixed");
    let (err, body) = &r.tool_results[1];
    assert!(!err, "the source edit is accepted: {body}");
    assert!(body.contains("<repro-confirmed>"), "{body}");
    assert!(
        body.contains("FAILED test_bug"),
        "the failing output reaches the model: {body}"
    );
}

#[test]
fn a_reproduction_that_passes_before_the_fix_is_rejected() {
    let r = run(
        repro_gate(FAKE_PYTEST, 2),
        10,
        vec![
            write("t1", "tests/test_bug.py", "passes-already"),
            write("s1", "src.py", "fixed"),
            write("t2", "tests/test_bug.py", "fails"),
            write("s2", "src.py", "fixed"),
            Block::text("done"),
        ],
    );
    assert_eq!(r.result.expect("loop ok").assistant_text, "done");
    let (err, body) = &r.tool_results[1];
    assert!(*err, "the first source edit is rejected: {body}");
    assert!(body.contains("PASSES on the current code"), "{body}");
    assert!(body.contains("Rewrite it"), "{body}");
    let (err, body) = &r.tool_results[3];
    assert!(!err && body.contains("<repro-confirmed>"), "{body}");
    assert_eq!(r.src, "fixed");
}

#[test]
fn a_reproduction_that_errors_at_collection_is_rejected_with_the_reason() {
    let r = run(
        repro_gate(FAKE_PYTEST, 2),
        10,
        vec![
            write("t1", "tests/test_bug.py", "collect-error"),
            write("s1", "src.py", "fixed"),
            write("t2", "tests/test_bug.py", "fails"),
            write("s2", "src.py", "fixed"),
            Block::text("done"),
        ],
    );
    assert_eq!(r.result.expect("loop ok").assistant_text, "done");
    let (err, body) = &r.tool_results[1];
    assert!(*err, "{body}");
    assert!(body.contains("errored before running a test"), "{body}");
    assert!(body.contains("ERROR collecting"), "the output is shown: {body}");
    assert_eq!(r.src, "fixed");
}

#[test]
fn a_spent_budget_proceeds_unconfirmed_and_says_so() {
    let r = run(
        repro_gate(FAKE_PYTEST, 1),
        10,
        vec![
            write("t1", "tests/test_bug.py", "passes-already"),
            write("s1", "src.py", "fixed"),
            write("s2", "src.py", "fixed"),
            Block::text("done"),
        ],
    );
    let text = r.result.expect("loop ok, no hard failure").assistant_text;
    assert!(text.starts_with("done"), "{text}");
    assert!(text.contains("WITHOUT a confirmed reproduction"), "{text}");
    let rejected = r.tool_results.iter().filter(|(e, _)| *e).count();
    assert_eq!(
        rejected, 1,
        "one rejection, then it proceeds: {:?}",
        r.tool_results
    );
    assert_eq!(r.src, "fixed");
}

#[test]
fn with_the_gate_off_a_passing_test_does_not_block_the_edit() {
    let cfg = PostEditConfig {
        auto_test: true,
        test_command: Some(FAKE_PYTEST.into()),
        ..PostEditConfig::default()
    };
    let r = run(
        cfg,
        10,
        vec![
            write("t1", "tests/test_bug.py", "passes-already"),
            write("s1", "src.py", "fixed"),
            Block::text("done"),
        ],
    );
    assert_eq!(r.result.expect("loop ok").assistant_text, "done");
    assert!(
        r.tool_results.iter().all(|(e, b)| !e && !b.contains("repro")),
        "{:?}",
        r.tool_results
    );
    assert_eq!(r.src, "fixed");
}

#[test]
fn a_red_gate_on_the_last_allowed_turn_returns_the_answer() {
    let r = run(
        repro_gate("echo 'FAILED always'; exit 1", 2),
        2,
        vec![write("s1", "src.py", "attempt"), Block::text("done")],
    );
    let summary = r.result.expect("Ok, not MaxTurns");
    assert_eq!(summary.assistant_text, "done");
    let signals = summary.gate_signals.expect("the RED gate is reported");
    assert!(signals.contains("FAILED always"), "{signals}");
}
