// SPDX-License-Identifier: Apache-2.0
//! C8: with checkpoints on, a turn-end gate that passes tags the current tree
//! as `last-green` in the run's shadow repo; a run that then ends with a failing
//! gate tells the user (and model) how to get back to it. Never restores.

#![allow(clippy::panic)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
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

/// Drive `run_loop` in `ws` with `checkpoints` on or off, the test gate
/// `gate`, one block per assistant turn. The user's global git config asks
/// for commit signing with a gpg that does not exist, so a checkpoint commit
/// that is not self-contained fails.
fn run(ws: &Path, checkpoints: bool, gate: &str, repairs: u32, turns: Vec<Block>) -> (String, Session) {
    let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let global = ws.join("hostile.gitconfig");
    std::fs::write(
        &global,
        "[commit]\n\tgpgsign = true\n[gpg]\n\tprogram = origin-no-such-gpg\n",
    )
    .expect("write gitconfig");
    std::env::set_var("GIT_CONFIG_GLOBAL", &global);
    std::env::set_var("ORIGIN_ENV_PASSTHROUGH", "GIT_CONFIG_GLOBAL");
    std::env::remove_var("ORIGIN_CHECKPOINTS_PER_TOOL");
    std::env::remove_var("ORIGIN_CMD_GUARD");
    if checkpoints {
        std::env::set_var("ORIGIN_CHECKPOINTS", "1");
    } else {
        std::env::remove_var("ORIGIN_CHECKPOINTS");
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
        post_edit: Some(Arc::new(PostEditConfig {
            auto_test: true,
            test_command: Some(gate.into()),
            max_repair_iters: repairs,
            ..PostEditConfig::default()
        })),
        ..LoopOptions::default().without_streaming()
    };
    let mut session = Session::new("last-green", "scripted-model");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let result = rt.block_on(run_loop(&mut session, "go", &provider, &AlwaysAllow, &opts));
    for k in [
        "ORIGIN_CHECKPOINTS",
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

fn texts(session: &Session, role: Role) -> Vec<String> {
    session
        .messages
        .iter()
        .filter(|m| m.role == role)
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
        "echo ok",
        0,
        vec![write("w1", "a.txt"), Block::text("done")],
    );
    let files = git(ws.path(), &["ls-tree", "--name-only", "HEAD"]).unwrap_or_default();
    assert!(
        files.lines().any(|l| l == "a.txt"),
        "the turn checkpoint was committed: {files:?}"
    );
}

#[test]
fn a_passing_gate_tags_the_current_tree_last_green() {
    let ws = workspace();
    run(
        ws.path(),
        true,
        GATE,
        0,
        vec![write("w1", "ok.txt"), Block::text("done")],
    );
    let files = git(ws.path(), &["ls-tree", "--name-only", TAG]).expect("last-green tag exists");
    assert!(files.lines().any(|l| l == "ok.txt"), "tagged tree: {files:?}");
}

#[test]
fn a_red_end_after_a_green_turn_names_the_restore_command_once() {
    let ws = workspace();
    let (text, session) = run(ws.path(), true, GATE, 1, green_then_red());
    let id = git(ws.path(), &["rev-parse", TAG]).expect("last-green tag exists");
    let files = git(ws.path(), &["ls-tree", "--name-only", &id]).unwrap_or_default();
    assert!(files.lines().any(|l| l == "ok.txt"), "the green tree: {files:?}");
    assert!(text.contains(NOTE), "final result carries the note: {text}");
    assert!(text.contains("turn 1"), "names when: {text}");
    // `origin rewind <id>` (origin-cli cli_def.rs `Rewind { id }`).
    assert!(
        text.contains(&format!("`origin rewind {id}`")),
        "names the restore command: {text}"
    );
    assert!(
        ws.path().join("ok.txt").metadata().is_err(),
        "nothing was restored"
    );
    // Once, on the final assistant message (what the model sees next), never
    // on the repro-gate continuation.
    let assistant: Vec<_> = texts(&session, Role::Assistant)
        .into_iter()
        .filter(|t| t.contains(NOTE))
        .collect();
    assert_eq!(assistant.len(), 1, "{assistant:?}");
    assert!(!texts(&session, Role::User).iter().any(|t| t.contains(NOTE)));
}

#[test]
fn no_green_state_no_note() {
    let ws = workspace();
    let (text, _) = run(
        ws.path(),
        true,
        GATE,
        0,
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
        "echo .env curl",
        0,
        vec![write("w1", "a.txt"), Block::text("done")],
    );
    assert!(
        git(ws.path(), &["rev-parse", "HEAD"]).is_some(),
        "checkpoints were on"
    );
    assert_eq!(git(ws.path(), &["rev-parse", "--verify", "-q", TAG]), None);
}

#[test]
fn a_last_green_from_before_the_run_is_not_mentioned() {
    let ws = workspace();
    std::fs::write(ws.path().join("ok.txt"), "x\n").expect("write");
    git(ws.path(), &["add", "-A"]).expect("add");
    git(ws.path(), &["commit", "-q", "-m", "earlier run"]).expect("commit");
    git(ws.path(), &["update-ref", TAG, "HEAD"]).expect("tag");
    let before = git(ws.path(), &["rev-parse", TAG]);
    std::fs::remove_file(ws.path().join("ok.txt")).expect("rm");
    let (text, _) = run(
        ws.path(),
        true,
        GATE,
        0,
        vec![write("w1", "other.txt"), Block::text("done")],
    );
    assert!(!text.contains(NOTE), "{text}");
    assert_eq!(git(ws.path(), &["rev-parse", TAG]), before);
}

#[test]
fn checkpoints_off_tags_nothing_and_adds_no_note() {
    let ws = workspace();
    let (text, _) = run(ws.path(), false, GATE, 1, green_then_red());
    assert!(!text.contains(NOTE), "{text}");
    assert_eq!(
        git(ws.path(), &["rev-parse", "--verify", "-q", "HEAD"]),
        None,
        "no shadow commit"
    );
    assert_eq!(git(ws.path(), &["rev-parse", "--verify", "-q", TAG]), None);
}
