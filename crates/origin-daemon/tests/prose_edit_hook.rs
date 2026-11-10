// SPDX-License-Identifier: Apache-2.0
//! C2: a prose edit fires the `PreTool` hook a `Write` to the same path would
//! fire, and a hook `Deny` refuses it. One test fn in its own binary: the hooks
//! runtime is built once per process from `$ORIGIN_HOME/.origin/hooks.json`.
#![allow(clippy::unwrap_used)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::Mutex;

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

/// A `PreTool` hook that denies any event mentioning `secrets`.
const DENY_SECRETS: &str = "require('readline').createInterface({input:process.stdin}).on('line',l=>{\
    process.stdout.write((l.includes('secrets')\
    ?JSON.stringify({override:{action:'deny',reason:'no writes under secrets/'}}):'{}')+'\\0');})";

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[tokio::test]
async fn a_pre_tool_hook_deny_refuses_a_prose_edit() {
    if !node_available() {
        eprintln!("skipping a_pre_tool_hook_deny_refuses_a_prose_edit: `node` unavailable");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".origin")).unwrap();
    let hooks = serde_json::json!({ "hooks": [
        { "event": "pre_tool", "program": "node", "args": ["-e", DENY_SECRETS] }
    ]});
    std::fs::write(home.path().join(".origin/hooks.json"), hooks.to_string()).unwrap();
    std::env::set_var("ORIGIN_HOME", home.path());
    std::env::set_var("ORIGIN_EDITFMT", "1");

    let ws = tempfile::tempdir().unwrap();
    for dir in ["secrets", "c2_prose"] {
        std::fs::create_dir_all(ws.path().join(dir)).unwrap();
    }
    let opts = LoopOptions {
        cwd: Some(ws.path().to_path_buf()),
        ..LoopOptions::default().without_streaming()
    };
    for (path, written) in [("secrets/key.txt", false), ("c2_prose/ok.txt", true)] {
        let provider = Reply(Mutex::new(Some(format!("{path}\n```\nx\n```\n"))));
        let mut session = Session::new_with_id("prose-hook".into(), "m".into());
        let summary = run_loop(&mut session, "edit", &provider, &AlwaysAllow, &opts)
            .await
            .unwrap();
        assert_eq!(
            ws.path().join(path).exists(),
            written,
            "{path}: {}",
            summary.assistant_text
        );
        if !written {
            assert!(
                summary
                    .assistant_text
                    .contains("\"secrets/key.txt\": hook denied: no writes under secrets/"),
                "{}",
                summary.assistant_text
            );
        }
    }
}
