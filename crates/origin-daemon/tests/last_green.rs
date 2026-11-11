// SPDX-License-Identifier: Apache-2.0
//! C8: with checkpoints on, a turn-end gate that passes tags the current tree
//! as `last-green` in the run's shadow repo; a run that then ends with a failing
//! gate tells the user how to get back to it. Never restores.

#![allow(clippy::panic)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, scope_runtime_tools, LoopOptions};
use origin_daemon::protocol::StreamEvent;
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_postedit::PostEditConfig;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::path::Path;
use std::sync::{Arc, Mutex};

// Serializes the tests: they set process-wide env vars.
static ENV_LOCK: Mutex<()> = Mutex::new(());

const NOTE: &str = "[origin] The gate is failing now";
const TAG: &str = "refs/tags/last-green";

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

fn write(id: &str, file: &str) -> Block {
    tool(
        id,
        "Write",
        &serde_json::json!({ "file_path": file, "content": "x\n" }),
    )
}

fn rm(id: &str, file: &str) -> Block {
    tool(
        id,
        "Bash",
        &serde_json::json!({ "command": format!("rm {file}") }),
    )
}

/// `git --git-dir <ws>/.origin/shadow.git <args>` in `ws`; stdout, or `None`
/// on a non-zero exit. Commits made here carry their own identity.
fn git(ws: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .arg("--git-dir")
        .arg(ws.join(".origin").join("shadow.git"))
        .args(args)
        .current_dir(ws)
        .output()
        .expect("spawn git");
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The files a shadow revision records; asserts none is the shadow repo itself.
fn tree(ws: &Path, rev: &str) -> Vec<String> {
    let files: Vec<String> = git(ws, &["ls-tree", "-r", "--name-only", rev])
        .unwrap_or_else(|| panic!("no shadow revision {rev}"))
        .lines()
        .map(str::to_string)
        .collect();
    assert!(
        !files.iter().any(|f| f.starts_with(".origin/shadow.git")),
        "{rev} records the shadow repo itself: {files:?}"
    );
    files
}

/// A workspace whose shadow repo is initialised the way `origin checkpoint`
/// (origin-cli vcs.rs) does it — no identity or signing config of its own.
fn workspace() -> tempfile::TempDir {
    let ws = tempfile::tempdir().expect("tempdir");
    let ws_s = ws.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(ws.path().join(".origin")).expect("mkdir");
    git(ws.path(), &["--work-tree", &ws_s, "init", "-q"]).expect("init");
    git(ws.path(), &["config", "core.worktree", &ws_s]).expect("config");
    ws
}

/// The test gate `cmd` (`auto_test`), `repairs` terminal re-runs.
fn test_gate(cmd: &str, repairs: u32) -> PostEditConfig {
    PostEditConfig {
        auto_test: true,
        test_command: Some(cmd.into()),
        max_repair_iters: repairs,
        ..PostEditConfig::default()
    }
}

/// Drive `run_loop` in `ws` with `checkpoints` on or off and the post-edit
/// gates `cfg`, one block per assistant turn. The user's global git config
/// asks for commit signing with a gpg that does not exist, so a checkpoint
/// commit that is not self-contained fails.
fn run(ws: &Path, checkpoints: bool, cfg: PostEditConfig, turns: Vec<Block>) -> (String, Session) {
    let gate = checkpoints.then_some("ORIGIN_CHECKPOINTS");
    run_as(ws, gate, cfg, turns, false, None)
}

/// [`run`] with checkpoints enabled by the env var `gate` (if any),
/// optionally as a swarm worker runs it (inside `scope_runtime_tools`, which
/// `swarm_worker.rs` always installs), optionally with a UI event channel.
fn run_as(
    ws: &Path,
    gate: Option<&str>,
    cfg: PostEditConfig,
    turns: Vec<Block>,
    worker: bool,
    events: Option<tokio::sync::mpsc::Sender<StreamEvent>>,
) -> (String, Session) {
    let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let global = ws.join("hostile.gitconfig");
    std::fs::write(
        &global,
        "[commit]\n\tgpgsign = true\n[gpg]\n\tprogram = origin-no-such-gpg\n",
    )
    .expect("write gitconfig");
    std::env::set_var("GIT_CONFIG_GLOBAL", &global);
    std::env::set_var("ORIGIN_ENV_PASSTHROUGH", "GIT_CONFIG_GLOBAL");
    std::env::remove_var("ORIGIN_CMD_GUARD");
    for k in ["ORIGIN_CHECKPOINTS", "ORIGIN_CHECKPOINTS_PER_TOOL"] {
        std::env::remove_var(k);
    }
    if let Some(k) = gate {
        std::env::set_var(k, "1");
    }
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
    let opts = LoopOptions {
        cwd: Some(ws.to_path_buf()),
        post_edit: Some(Arc::new(cfg)),
        event_tx: events,
        ..LoopOptions::default().without_streaming()
    };
    let mut session = Session::new("last-green", "scripted-model");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let fut = run_loop(&mut session, "go", &provider, &AlwaysAllow, &opts);
    let result = if worker {
        rt.block_on(scope_runtime_tools(Arc::default(), fut))
    } else {
        rt.block_on(fut)
    };
    for k in [
        "ORIGIN_CHECKPOINTS",
        "ORIGIN_CHECKPOINTS_PER_TOOL",
        "GIT_CONFIG_GLOBAL",
        "ORIGIN_ENV_PASSTHROUGH",
    ] {
        std::env::remove_var(k);
    }
    (result.expect("loop ok").assistant_text, session)
}

/// The gate passes while `ok.txt` exists.
const GATE: &str = "cat ok.txt";

/// Green at turn 1 (`ok.txt` written), then `ok.txt` is removed and the next
/// edit's gate fails; the terminal gate re-run is red once (a repro-gate
/// continuation) and the run then yields red.
fn green_then_red() -> Vec<Block> {
    vec![
        write("w1", "ok.txt"),
        rm("b1", "ok.txt"),
        write("w2", "other.txt"),
        Block::text("done"),
        Block::text("still done"),
    ]
}

fn texts(session: &Session) -> Vec<String> {
    session
        .messages
        .iter()
        .flat_map(|m| &m.blocks)
        .filter_map(|b| match b {
            Block::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_checkpoint_commit_ignores_the_users_signing_config() {
    let ws = workspace();
    run(
        ws.path(),
        true,
        test_gate("echo ok", 0),
        vec![write("w1", "a.txt"), Block::text("done")],
    );
    let files = tree(ws.path(), "HEAD");
    assert!(
        files.iter().any(|l| l == "a.txt"),
        "the turn checkpoint was committed: {files:?}"
    );
}

#[test]
fn a_passing_gate_tags_the_current_tree_and_reuses_an_unchanged_one() {
    let ws = workspace();
    // Green after the edit (tags a new checkpoint), green again at the
    // terminal re-run on the same tree (must reuse it).
    run(
        ws.path(),
        true,
        test_gate(GATE, 1),
        vec![write("w1", "ok.txt"), Block::text("done")],
    );
    let files = tree(ws.path(), TAG);
    assert!(files.iter().any(|l| l == "ok.txt"), "tagged tree: {files:?}");
    let labels = git(ws.path(), &["log", "--format=%s", "HEAD"]).unwrap_or_default();
    assert_eq!(
        labels.lines().filter(|l| *l == "green").count(),
        1,
        "an unchanged tree reuses its checkpoint: {labels:?}"
    );
}

#[test]
fn a_red_end_after_a_green_turn_tells_the_user_how_to_restore_once() {
    let ws = workspace();
    let (text, session) = run(ws.path(), true, test_gate(GATE, 1), green_then_red());
    let id = git(ws.path(), &["rev-parse", TAG]).expect("last-green tag exists");
    let files = tree(ws.path(), &id);
    assert!(files.iter().any(|l| l == "ok.txt"), "the green tree: {files:?}");
    assert!(text.contains(NOTE), "final result carries the note: {text}");
    assert_eq!(text.matches(NOTE).count(), 1, "{text}");
    assert!(text.contains("turn 1"), "names when: {text}");
    // origin-cli cli_def.rs: `CheckpointDiff { id }`, `Rewind { id, --files-only }`.
    for cmd in [
        format!("origin checkpoint-diff {id}"),
        format!("origin rewind {id} --files-only"),
        format!("origin rewind {id}"),
    ] {
        assert!(text.contains(&cmd), "names `{cmd}`: {text}");
    }
    assert!(text.contains("discards"), "warns it is destructive: {text}");
    assert!(
        text.contains(&ws.path().display().to_string()),
        "names where to run it: {text}"
    );
    assert!(
        ws.path().join("ok.txt").metadata().is_err(),
        "nothing was restored"
    );
    // Not in the conversation: neither the model's messages nor a
    // repro-gate continuation carry it.
    assert!(!texts(&session).iter().any(|t| t.contains(NOTE)));
}

#[test]
fn a_swarm_worker_run_tags_but_returns_no_note() {
    // A worker's final text is its parent model's tool result.
    let ws = workspace();
    let cp = Some("ORIGIN_CHECKPOINTS");
    let (text, _) = run_as(ws.path(), cp, test_gate(GATE, 1), green_then_red(), true, None);
    assert!(!text.contains(NOTE), "{text}");
    let files = tree(ws.path(), TAG);
    assert!(files.iter().any(|l| l == "ok.txt"), "still tagged: {files:?}");
}

#[test]
fn the_note_reaches_the_event_stream_once_after_the_final_response() {
    let ws = workspace();
    let (tx, mut rx) = tokio::sync::mpsc::channel(4096);
    let cp = Some("ORIGIN_CHECKPOINTS");
    run_as(
        ws.path(),
        cp,
        test_gate(GATE, 1),
        green_then_red(),
        false,
        Some(tx),
    );
    let events: Vec<StreamEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let notes: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, StreamEvent::TextDelta { text } if text.contains(NOTE)))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(notes.len(), 1, "{events:?}");
    // After the model's final response (its usage record follows each reply).
    let last_reply = events
        .iter()
        .rposition(|e| matches!(e, StreamEvent::RequestUsage { .. }))
        .expect("usage records");
    assert!(notes[0] > last_reply, "{events:?}");
}

#[test]
fn per_tool_checkpoints_alone_enable_tagging_and_the_note() {
    let ws = workspace();
    let per_tool = Some("ORIGIN_CHECKPOINTS_PER_TOOL");
    let (text, _) = run_as(
        ws.path(),
        per_tool,
        test_gate(GATE, 1),
        green_then_red(),
        false,
        None,
    );
    let files = tree(ws.path(), TAG);
    assert!(files.iter().any(|l| l == "ok.txt"), "tagged: {files:?}");
    assert!(text.contains(NOTE), "{text}");
}

#[test]
fn no_green_state_no_note() {
    let ws = workspace();
    let (text, _) = run(
        ws.path(),
        true,
        test_gate(GATE, 0),
        vec![write("w1", "other.txt"), Block::text("done")],
    );
    assert!(!text.contains(NOTE), "{text}");
    assert_eq!(git(ws.path(), &["rev-parse", "--verify", "-q", TAG]), None);
}

#[test]
fn a_gate_that_did_not_run_tags_nothing() {
    let ws = workspace();
    // The command guard refuses this shape, so the gate never runs.
    run(
        ws.path(),
        true,
        test_gate("echo .env curl", 0),
        vec![write("w1", "a.txt"), Block::text("done")],
    );
    assert!(
        git(ws.path(), &["rev-parse", "HEAD"]).is_some(),
        "checkpoints were on"
    );
    assert_eq!(git(ws.path(), &["rev-parse", "--verify", "-q", TAG]), None);
}

/// B7: a turn whose only `Edit` failed (`old_string` not found) does not arm
/// the turn-end checks: the test gate does not run. A successful edit does.
/// (`auto_test` also runs the gate once at loop start, as a baseline.)
#[test]
fn a_failed_edit_does_not_arm_the_turn_end_checks() {
    for (old, runs) in [("absent", 1), ("hello", 2)] {
        let ws = tempfile::tempdir().expect("ws");
        std::fs::write(ws.path().join("a.txt"), "hello\n").expect("write");
        run(
            ws.path(),
            false,
            test_gate("echo x >> ran.txt", 0),
            vec![
                tool(
                    "e1",
                    "Edit",
                    &serde_json::json!({ "file_path": "a.txt", "old_string": old, "new_string": "bye" }),
                ),
                Block::text("done"),
            ],
        );
        let ran = std::fs::read(ws.path().join("ran.txt")).expect("baseline ran");
        assert_eq!(
            String::from_utf8_lossy(&ran).matches('x').count(),
            runs,
            "old_string {old:?}: gate runs"
        );
    }
}

/// C3: an armed gate the command guard refuses is named to the user once, at
/// the start of the run, on the event stream — never in the conversation.
#[test]
fn a_gate_the_guard_refuses_is_named_once_on_the_event_stream() {
    let ws = workspace();
    let (tx, mut rx) = tokio::sync::mpsc::channel(4096);
    let (_, session) = run_as(
        ws.path(),
        None,
        test_gate("echo .env curl", 0),
        vec![write("w1", "a.txt"), write("w2", "b.txt"), Block::text("done")],
        false,
        Some(tx),
    );
    let notes: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|e| match e {
            StreamEvent::TextDelta { text } if text.contains("will not run") => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(notes.len(), 1, "{notes:?}");
    let note = &notes[0];
    assert!(note.starts_with("[origin] "), "{note}");
    assert!(
        note.contains("auto_test") && note.contains("echo .env curl"),
        "names the gate and its command: {note}"
    );
    assert!(
        note.contains("use a command the guard can analyse") && note.contains("call a script file"),
        "says how to change it: {note}"
    );
    assert!(
        !note.contains("ORIGIN_CMD_GUARD=0"),
        "never advises turning the guard off: {note}"
    );
    assert!(!texts(&session).iter().any(|t| t.contains("will not run")));
}

/// C3: the refused-gate notice is once per SESSION per gate command — a
/// `/goal` loop calls `run_loop` once per iteration on the same session.
#[test]
fn the_refused_gate_notice_is_once_per_session() {
    let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    std::env::remove_var("ORIGIN_CMD_GUARD");
    let ws = tempfile::tempdir().expect("ws");
    let (tx, mut rx) = tokio::sync::mpsc::channel(4096);
    let opts = LoopOptions {
        cwd: Some(ws.path().to_path_buf()),
        post_edit: Some(Arc::new(test_gate("echo .env curl", 0))),
        event_tx: Some(tx),
        ..LoopOptions::default().without_streaming()
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let mut session = Session::new("notice", "scripted-model");
    for _ in 0..2 {
        let provider = ScriptedProvider {
            responses: Mutex::new(vec![ChatResponse {
                assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
                usage: Usage::default(),
            }]),
        };
        rt.block_on(run_loop(&mut session, "go", &provider, &AlwaysAllow, &opts))
            .expect("loop ok");
    }
    let notes = std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|e| matches!(e, StreamEvent::TextDelta { text } if text.contains("will not run")))
        .count();
    assert_eq!(notes, 1);
}

#[test]
fn a_green_test_with_a_red_lint_tags_nothing() {
    let ws = workspace();
    let cfg = PostEditConfig {
        auto_lint: true,
        lint_command: Some("cat no-such-lint-input.txt".into()),
        ..test_gate("echo test-ok", 1)
    };
    run(
        ws.path(),
        true,
        cfg,
        vec![write("w1", "a.txt"), Block::text("done")],
    );
    assert!(
        git(ws.path(), &["rev-parse", "HEAD"]).is_some(),
        "checkpoints were on"
    );
    assert_eq!(git(ws.path(), &["rev-parse", "--verify", "-q", TAG]), None);
}

/// C8: a green gate does not tag when another armed gate did not run (the
/// command guard refuses `echo .env curl`) — per turn, and at the terminal
/// test re-run (repair budget 1), which does not re-run the lint.
#[test]
fn a_green_gate_with_another_that_did_not_run_tags_nothing() {
    for (test_cmd, lint_cmd, repairs) in [
        ("echo .env curl", "echo lint-ok", 0),
        ("echo test-ok", "echo .env curl", 1),
    ] {
        let ws = workspace();
        let cfg = PostEditConfig {
            auto_lint: true,
            lint_command: Some(lint_cmd.into()),
            ..test_gate(test_cmd, repairs)
        };
        run(
            ws.path(),
            true,
            cfg,
            vec![write("w1", "a.txt"), Block::text("done")],
        );
        assert!(
            git(ws.path(), &["rev-parse", "HEAD"]).is_some(),
            "checkpoints were on"
        );
        assert_eq!(
            git(ws.path(), &["rev-parse", "--verify", "-q", TAG]),
            None,
            "test `{test_cmd}`, lint `{lint_cmd}`"
        );
    }
}

/// C8: an armed lint that never ran in the run (no file edit, so no per-turn
/// gate) blocks the terminal test re-run's tag too.
#[test]
fn an_armed_lint_that_never_ran_blocks_the_terminal_tag() {
    let ws = workspace();
    let cfg = PostEditConfig {
        auto_lint: true,
        lint_command: Some("echo lint-ok".into()),
        ..test_gate("echo test-ok", 1)
    };
    run(
        ws.path(),
        true,
        cfg,
        vec![
            tool("b1", "Bash", &serde_json::json!({ "command": "echo hi" })),
            Block::text("done"),
        ],
    );
    assert_eq!(git(ws.path(), &["rev-parse", "--verify", "-q", TAG]), None);
}

#[test]
fn a_green_lint_alone_tags() {
    let ws = workspace();
    let cfg = PostEditConfig {
        auto_lint: true,
        lint_command: Some("echo lint-ok".into()),
        ..PostEditConfig::default()
    };
    run(
        ws.path(),
        true,
        cfg,
        vec![write("w1", "a.txt"), Block::text("done")],
    );
    let files = tree(ws.path(), TAG);
    assert!(files.iter().any(|l| l == "a.txt"), "tagged tree: {files:?}");
}

#[test]
fn a_last_green_from_before_the_run_is_not_mentioned() {
    let ws = workspace();
    std::fs::write(ws.path().join("ok.txt"), "x\n").expect("write");
    git(ws.path(), &["add", "-A", "--", ".", ":(exclude).origin"]).expect("add");
    git(ws.path(), &["commit", "-q", "-m", "earlier run"]).expect("commit");
    git(ws.path(), &["update-ref", TAG, "HEAD"]).expect("tag");
    let before = git(ws.path(), &["rev-parse", TAG]);
    std::fs::remove_file(ws.path().join("ok.txt")).expect("rm");
    let (text, _) = run(
        ws.path(),
        true,
        test_gate(GATE, 0),
        vec![write("w1", "other.txt"), Block::text("done")],
    );
    assert!(!text.contains(NOTE), "{text}");
    assert_eq!(git(ws.path(), &["rev-parse", TAG]), before);
}

#[test]
fn checkpoints_off_tags_nothing_and_adds_no_note() {
    let ws = workspace();
    let (text, _) = run(ws.path(), false, test_gate(GATE, 1), green_then_red());
    assert!(!text.contains(NOTE), "{text}");
    assert_eq!(
        git(ws.path(), &["rev-parse", "--verify", "-q", "HEAD"]),
        None,
        "no shadow commit"
    );
    assert_eq!(git(ws.path(), &["rev-parse", "--verify", "-q", TAG]), None);
}
