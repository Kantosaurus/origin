// SPDX-License-Identifier: Apache-2.0
//! A12: a run with its own cwd (a best-of-N candidate) never touches the pooled
//! language server, but it still collects the paths it edits: with
//! `ORIGIN_LSP_DIAGNOSTICS=1`, the syntax gate off and `auto_test` on, the
//! per-turn `<test-results>` block still appears. Own test binary because it
//! sets process-wide env vars.
#![allow(clippy::unwrap_used)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_postedit::PostEditConfig;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::{Arc, Mutex};

struct Recording {
    responses: Mutex<Vec<ChatResponse>>,
    seen: Mutex<Vec<Vec<Message>>>,
}

#[async_trait]
impl Provider for Recording {
    fn name(&self) -> &'static str {
        "recording"
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.seen.lock().unwrap().push(req.messages);
        let mut q = self.responses.lock().unwrap();
        if q.is_empty() {
            return Err(ProviderError::Api("no more scripted responses".into()));
        }
        Ok(q.remove(0))
    }
}

fn reply(b: Block) -> ChatResponse {
    ChatResponse {
        assistant: Message::new(Role::Assistant).with_block(b),
        usage: Usage::default(),
    }
}

#[tokio::test]
async fn a_cwd_run_with_lsp_on_and_the_syntax_gate_off_still_gets_per_turn_test_results() {
    let ws = tempfile::tempdir().unwrap();
    let write = Block::ToolUse {
        id: "tu_w".into(),
        name: "Write".into(),
        input_json: serde_json::to_vec(&serde_json::json!({"file_path": "a.txt", "content": "x\n"})).unwrap(),
        cache_marker: None,
    };
    let provider = Recording {
        responses: Mutex::new(vec![reply(write), reply(Block::text("done"))]),
        seen: Mutex::new(Vec::new()),
    };
    let opts = LoopOptions {
        cwd: Some(ws.path().to_path_buf()),
        post_edit: Some(Arc::new(PostEditConfig {
            auto_test: true,
            test_command: Some("echo a12-red; exit 1".into()),
            ..PostEditConfig::default()
        })),
        ..LoopOptions::default().without_streaming()
    };
    std::env::set_var("ORIGIN_LSP_DIAGNOSTICS", "1");
    std::env::set_var("ORIGIN_EDIT_CHECK", "0");
    std::env::remove_var("ORIGIN_CMD_GUARD");
    let mut session = Session::new("test", "claude-opus-4-7");
    // The terminal repro gate then sees the same RED suite and asks for a third
    // turn the script does not have; only the second request matters here.
    let _ = run_loop(&mut session, "write it", &provider, &AlwaysAllow, &opts).await;

    let seen = provider.seen.lock().unwrap().clone();
    let second = seen[1]
        .iter()
        .flat_map(|m| &m.blocks)
        .filter_map(|b| match b {
            Block::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        second.contains("<test-results>"),
        "the per-turn test gate never ran: {second}"
    );
}
