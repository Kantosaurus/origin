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
use origin_daemon::protocol::StreamEvent;
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

/// A pytest stand-in (a full-suite command: it is not narrowed): exit 2
/// (collection error) when the reproduction test says `collect-error`, exit 1
/// (a failing test) when it says `fails` and the source is not `fixed` yet,
/// else exit 0. Every run appends a line to `runs.log`.
#[cfg(unix)]
const FAKE_PYTEST: &str = "echo x >> runs.log; if grep -q collect-error tests/test_bug.py; then echo 'ERROR \
     collecting tests/test_bug.py pytest'; exit 2; fi; if grep -q fails tests/test_bug.py; then if grep -q fixed src.py; then \
     echo 'passed pytest'; else echo 'FAILED test_bug pytest'; exit 1; fi; fi; echo 'passed pytest'";
#[cfg(windows)]
const FAKE_PYTEST: &str =
    "Add-Content runs.log x; if (Select-String -Quiet collect-error tests/test_bug.py) { \
     echo 'ERROR collecting tests/test_bug.py pytest'; exit 2 }; if ((Select-String -Quiet fails tests/test_bug.py) -and -not \
     (Select-String -Quiet fixed src.py)) { echo 'FAILED test_bug pytest'; exit 1 }; echo 'passed pytest'";

struct Run {
    result: Result<LoopSummary, LoopError>,
    /// `(is_error, body)` of every tool result, in order.
    tool_results: Vec<(bool, String)>,
    /// Text of every user message after the prompt (continuations).
    user_texts: Vec<String>,
    src: String,
    /// How many times the test command ran.
    runs: usize,
    /// Stream events, when the run had an event channel.
    events: Vec<StreamEvent>,
}

impl Run {
    fn rejected(&self) -> usize {
        self.tool_results.iter().filter(|(e, _)| *e).count()
    }
}

fn run(cfg: PostEditConfig, max_turns: u32, turns: Vec<Block>) -> Run {
    run_turns(cfg, max_turns, turns.into_iter().map(|b| vec![b]).collect())
}

/// [`run`] with any number of blocks per assistant turn.
fn run_turns(cfg: PostEditConfig, max_turns: u32, turns: Vec<Vec<Block>>) -> Run {
    run_with(cfg, max_turns, turns, false)
}

/// [`run_turns`], optionally with an event channel whose events are kept.
fn run_with(cfg: PostEditConfig, max_turns: u32, turns: Vec<Vec<Block>>, with_events: bool) -> Run {
    let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    std::env::remove_var("ORIGIN_CMD_GUARD");
    std::env::remove_var("ORIGIN_TEST_SELECT");
    std::env::remove_var("ORIGIN_PRECOMPLETE_CHECK");
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("tests")).expect("mkdir");
    std::fs::write(dir.path().join("src.py"), "buggy\n").expect("seed");
    // Large enough that the loop never waits on a full channel (drained after).
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<StreamEvent>(10_000);
    let opts = LoopOptions {
        max_turns,
        cwd: Some(dir.path().to_path_buf()),
        post_edit: Some(Arc::new(cfg)),
        event_tx: with_events.then_some(event_tx),
        ..LoopOptions::default().without_streaming()
    };
    let provider = ScriptedProvider {
        responses: Mutex::new(
            turns
                .into_iter()
                .map(|blocks| ChatResponse {
                    assistant: blocks
                        .into_iter()
                        .fold(Message::new(Role::Assistant), Message::with_block),
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
    let user_texts = session
        .messages
        .iter()
        .filter(|m| m.role == Role::User)
        .flat_map(|m| &m.blocks)
        .filter_map(|b| match b {
            Block::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    let src = std::fs::read_to_string(dir.path().join("src.py")).expect("src");
    let runs = std::fs::read_to_string(dir.path().join("runs.log")).map_or(0, |s| s.lines().count());
    drop(opts);
    let mut events = Vec::new();
    while let Ok(e) = event_rx.try_recv() {
        events.push(e);
    }
    Run {
        result,
        tool_results,
        user_texts,
        src,
        runs,
        events,
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
    assert!(
        summary.assistant_text.starts_with("done"),
        "{}",
        summary.assistant_text
    );
    let signals = summary.gate_signals.expect("the RED gate is reported");
    assert!(signals.contains("FAILED always"), "{signals}");
}

#[test]
fn several_source_edits_in_one_turn_cost_one_run_and_one_charge() {
    let r = run_turns(
        repro_gate(FAKE_PYTEST, 2),
        10,
        vec![
            vec![write("t1", "tests/test_bug.py", "passes-already")],
            vec![
                write("s1", "src.py", "fixed"),
                write("s2", "src.py", "fixed"),
                write("s3", "src.py", "fixed"),
            ],
            vec![write("s4", "src.py", "fixed")],
            vec![write("s5", "src.py", "fixed")],
            vec![Block::text("done")],
        ],
    );
    let text = r.result.expect("loop ok").assistant_text;
    // Per-edit charging would have spent the budget in turn 2 and applied s3.
    let errs: Vec<bool> = r.tool_results.iter().map(|(e, _)| *e).collect();
    assert_eq!(
        errs,
        [false, true, true, true, true, false],
        "{:?}",
        r.tool_results
    );
    assert_eq!(r.tool_results[2].1, r.tool_results[1].1, "same reason, no re-run");
    assert!(text.contains("WITHOUT a confirmed reproduction"), "{text}");
    // Turn 1's turn-end run, ONE confirmation in turn 2, one each in turns 3
    // and 4, turn 4's turn-end run, and the terminal gate.
    assert_eq!(r.runs, 6);
}

#[test]
fn a_source_edit_before_any_reproduction_disarms_the_gate_and_says_so() {
    let r = run(
        repro_gate(FAKE_PYTEST, 2),
        10,
        vec![
            write("s1", "src.py", "fixed"),
            write("t1", "tests/test_bug.py", "passes-already"),
            write("s2", "src.py", "fixed"),
            Block::text("done"),
        ],
    );
    assert_eq!(r.rejected(), 0, "{:?}", r.tool_results);
    let text = r.result.expect("loop ok").assistant_text;
    assert!(text.contains("no reproduction test was written"), "{text}");
}

#[test]
fn a_test_and_source_patch_in_one_call_is_not_a_confirmed_reproduction() {
    let patch = "*** Begin Patch\n*** Add File: tests/test_bug.py\n+fails\n*** Update File: src.py\n\
                 @@ -1,1 +1,1 @@\n-buggy\n+fixed\n*** End Patch\n";
    let r = run(
        repro_gate(FAKE_PYTEST, 2),
        10,
        vec![
            Block::ToolUse {
                id: "p1".into(),
                name: "ApplyPatch".into(),
                input_json: serde_json::to_vec(&serde_json::json!({ "patch": patch })).expect("json"),
                cache_marker: None,
            },
            Block::text("done"),
        ],
    );
    assert_eq!(r.rejected(), 0, "{:?}", r.tool_results);
    assert_eq!(r.src.trim_end(), "fixed");
    let text = r.result.expect("loop ok").assistant_text;
    assert!(text.contains("no reproduction test was written"), "{text}");
}

#[test]
fn pre_edit_rejections_do_not_spend_the_terminal_gate_budget() {
    let r = run(
        repro_gate(FAKE_PYTEST, 2),
        12,
        vec![
            write("t1", "tests/test_bug.py", "collect-error"),
            write("s1", "src.py", "attempt"),
            write("s2", "src.py", "attempt"),
            write("s3", "src.py", "attempt"),
            Block::text("done"),
            Block::text("done again"),
            Block::text("final"),
        ],
    );
    assert_eq!(r.rejected(), 2, "{:?}", r.tool_results);
    let cont = r
        .user_texts
        .iter()
        .filter(|t| t.contains("You indicated you were finished"))
        .count();
    assert_eq!(cont, 2, "the terminal gate still has its full budget");
    assert!(r.result.expect("loop ok").assistant_text.starts_with("final"));
}

#[test]
fn an_unrelated_failure_in_a_full_suite_run_does_not_confirm() {
    let r = run(
        repro_gate("echo 'FAILED tests/test_old.py::test_legacy pytest'; exit 1", 2),
        12,
        vec![
            write(
                "t1",
                "tests/test_bug.py",
                "def test_reproduces():\n    assert False\n",
            ),
            write("s1", "src.py", "fixed"),
            Block::text("done"),
            Block::text("done"),
            Block::text("done"),
        ],
    );
    let (err, body) = &r.tool_results[1];
    assert!(*err, "{body}");
    assert!(body.contains("never names your reproduction test"), "{body}");
}

#[test]
fn an_unclear_runner_failing_is_accepted_with_a_caution() {
    let r = run(
        repro_gate("echo 'npm ERR! test_bug failed'; exit 1", 2),
        12,
        vec![
            write("t1", "tests/test_bug.py", "fails"),
            write("s1", "src.py", "fixed"),
            Block::text("done"),
            Block::text("done"),
            Block::text("done"),
        ],
    );
    let (err, body) = &r.tool_results[1];
    assert!(!err, "{body}");
    assert!(body.contains("<repro-confirmed>"), "{body}");
    assert!(
        body.contains("cannot tell a failing test from one that never ran"),
        "{body}"
    );
    assert_eq!(r.src, "fixed");
}

/// The first two lines of a full-suite run: the reproduction on a progress
/// line (it passed), another test failing.
fn full_suite_run(repro_line: &str) -> PostEditConfig {
    repro_gate(
        &format!("echo '{repro_line}'; echo 'FAILED tests/test_old.py::test_legacy pytest'; exit 1"),
        2,
    )
}

fn repro_then_fix() -> Vec<Block> {
    vec![
        write(
            "t1",
            "tests/test_bug.py",
            "def test_reproduces():\n    assert False\n",
        ),
        write("s1", "src.py", "fixed"),
        Block::text("done"),
        Block::text("done"),
        Block::text("done"),
    ]
}

#[test]
fn a_reproduction_named_only_on_a_progress_line_does_not_confirm() {
    let r = run(
        full_suite_run("tests/test_bug.py .   [ 50%]"),
        12,
        repro_then_fix(),
    );
    let (err, body) = &r.tool_results[1];
    assert!(*err, "{body}");
    assert!(body.contains("never names your reproduction test"), "{body}");
}

#[test]
fn a_reproduction_named_on_a_failure_line_confirms() {
    let r = run(
        full_suite_run("FAILED tests/test_bug.py::test_reproduces"),
        12,
        repro_then_fix(),
    );
    let (err, body) = &r.tool_results[1];
    assert!(!err, "{body}");
    assert!(body.contains("<repro-confirmed>"), "{body}");
}

#[test]
fn rewriting_the_test_in_the_same_turn_gets_a_fresh_confirmation() {
    let r = run_turns(
        repro_gate(FAKE_PYTEST, 2),
        10,
        vec![
            vec![write("t1", "tests/test_bug.py", "passes-already")],
            vec![
                write("s1", "src.py", "fixed"),
                write("t2", "tests/test_bug.py", "fails"),
                write("s2", "src.py", "fixed"),
            ],
            vec![Block::text("done")],
        ],
    );
    let errs: Vec<bool> = r.tool_results.iter().map(|(e, _)| *e).collect();
    assert_eq!(errs, [false, true, false, false], "{:?}", r.tool_results);
    assert!(r.tool_results[3].1.contains("<repro-confirmed>"));
    assert_eq!(r.src, "fixed");
}

#[test]
fn a_rejected_edit_emits_a_failed_tool_result_event() {
    let r = run_with(
        repro_gate(FAKE_PYTEST, 2),
        10,
        vec![
            vec![write("t1", "tests/test_bug.py", "passes-already")],
            vec![write("s1", "src.py", "fixed")],
            vec![write("t2", "tests/test_bug.py", "fails")],
            vec![write("s2", "src.py", "fixed")],
            vec![Block::text("done")],
        ],
        true,
    );
    let failed: Vec<&str> = r
        .events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::ToolResult {
                ok: false, preview, ..
            } => Some(preview.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(
        failed[0].contains("[repro-gate] edit NOT applied"),
        "{}",
        failed[0]
    );
}
