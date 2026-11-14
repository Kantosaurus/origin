// SPDX-License-Identifier: Apache-2.0
//! C2: a prose edit (`ORIGIN_EDITFMT=1`) lands only where a `Write` to the same
//! path, anchored at the run's working directory, would be allowed without
//! asking. Drives the real `run_loop` with a scripted provider; no network.
#![allow(clippy::panic, clippy::unwrap_used)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::protocol::PermissionMode;
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::path::Path;
use std::sync::Mutex;

/// `ORIGIN_EDITFMT` is process-global: every test here holds this.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Answers the one main-loop request with `reply` (no tool calls).
struct Reply(Mutex<Option<String>>);

#[async_trait]
impl Provider for Reply {
    fn name(&self) -> &'static str {
        "scripted"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let text = self.0.lock().unwrap().take().unwrap_or_else(|| "done".into());
        Ok(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text(text)),
            usage: Usage::default(),
        })
    }
}

/// Run one turn whose reply is `reply`, in `cwd`, under `mode`. The prompter
/// would allow anything: the applier must not ask it. Returns the final text.
async fn run(reply: String, cwd: &Path, mode: PermissionMode) -> String {
    let provider = Reply(Mutex::new(Some(reply)));
    let opts = LoopOptions {
        cwd: Some(cwd.to_path_buf()),
        permission_mode: mode,
        ..LoopOptions::default().without_streaming()
    };
    let mut session = Session::new_with_id("prose-c2".into(), "m".into());
    std::env::set_var("ORIGIN_EDITFMT", "1");
    let out = run_loop(&mut session, "edit", &provider, &AlwaysAllow, &opts).await;
    std::env::remove_var("ORIGIN_EDITFMT");
    out.unwrap().assistant_text
}

/// C2: a refused prose edit is named in the run's final text (the model gets
/// no further turn to say so); an applied one adds nothing.
#[tokio::test]
async fn a_refused_prose_edit_is_named_in_the_final_text() {
    let _g = ENV_LOCK.lock().await;
    let ws = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(ws.path().join(".git/hooks")).unwrap();
    std::fs::create_dir_all(ws.path().join("c2_prose")).unwrap();
    let refused = whole_file(".git/hooks/pre-commit", "#!/bin/sh");
    let text = run(refused.clone(), ws.path(), PermissionMode::Workspace).await;
    assert!(
        text.starts_with(&refused)
            && text.ends_with(
                "\n\n[editfmt] 1 prose edit(s) not applied: \".git/hooks/pre-commit\": not permitted \
                 without asking; use the edit tools"
            ),
        "{text}"
    );
    let applied = whole_file("c2_prose/ok.txt", "x");
    let text = run(applied.clone(), ws.path(), PermissionMode::Workspace).await;
    assert_eq!(text, applied, "no note for an applied edit");
}

fn whole_file(path: &str, body: &str) -> String {
    format!("{path}\n```\n{body}\n```\n")
}

fn search_replace(path: &str, before: &str, after: &str) -> String {
    format!("{path}\n<<<<<<< SEARCH\n{before}\n=======\n{after}\n>>>>>>> REPLACE\n")
}

#[tokio::test]
async fn workspace_mode_writes_inside_and_refuses_protected_and_outside_paths() {
    let _g = ENV_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join(".git/hooks")).unwrap();
    let sibling = tmp.path().join("outside.txt");
    std::fs::write(&sibling, "old\n").unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let absolute = elsewhere.path().join("abs.txt");
    std::fs::write(&absolute, "old\n").unwrap();

    // A relative in-workspace file lands in the RUN's directory, not the
    // process's (`c2_prose/` exists only in the workspace).
    std::fs::create_dir_all(ws.join("c2_prose")).unwrap();
    run(
        whole_file("c2_prose/new.rs", "fn a() {}"),
        &ws,
        PermissionMode::Workspace,
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(ws.join("c2_prose/new.rs"))
            .ok()
            .as_deref(),
        Some("fn a() {}")
    );
    assert!(!Path::new("c2_prose").exists(), "not in the process cwd");

    run(
        whole_file(".git/hooks/pre-commit", "#!/bin/sh\nrm -rf /"),
        &ws,
        PermissionMode::Workspace,
    )
    .await;
    assert!(
        !ws.join(".git/hooks/pre-commit").exists(),
        "protected path written"
    );

    run(
        search_replace("../outside.txt", "old", "new"),
        &ws,
        PermissionMode::Workspace,
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(&sibling).unwrap(),
        "old\n",
        "`..` escape written"
    );

    let abs = absolute.to_string_lossy().into_owned();
    run(search_replace(&abs, "old", "new"), &ws, PermissionMode::Workspace).await;
    assert_eq!(
        std::fs::read_to_string(&absolute).unwrap(),
        "old\n",
        "absolute outside written"
    );
}

#[tokio::test]
async fn mode_on_writes_nothing_and_mode_off_writes_as_before() {
    let _g = ENV_LOCK.lock().await;
    let ws = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(ws.path().join("c2_prose")).unwrap();
    run(whole_file("c2_prose/on.txt", "x"), ws.path(), PermissionMode::On).await;
    assert!(
        !ws.path().join("c2_prose/on.txt").exists(),
        "mode on: a prose edit cannot ask"
    );
    run(
        whole_file("c2_prose/off.txt", "x"),
        ws.path(),
        PermissionMode::Off,
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(ws.path().join("c2_prose/off.txt"))
            .ok()
            .as_deref(),
        Some("x")
    );
}
