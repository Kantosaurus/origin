// SPDX-License-Identifier: Apache-2.0
//! C2 (final fix S2): `LspNavigate` and `Diagnostics` start a language server
//! on the workspace (which runs build scripts and proc-macros), so they are
//! gated: in `workspace` mode a run that cannot ask denies them before any
//! server starts; mode `off` dispatches them.

#![allow(clippy::panic)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::protocol::PermissionMode;
use origin_daemon::session::Session;
use origin_permission::prompt::NoPrompter;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::Mutex;

struct ScriptedProvider(Mutex<Vec<ChatResponse>>);

#[async_trait]
impl Provider for ScriptedProvider {
    fn name(&self) -> &'static str {
        "scripted"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let mut q = self.0.lock().expect("lock");
        if q.is_empty() {
            return Err(ProviderError::Api("no more scripted responses".into()));
        }
        Ok(q.remove(0))
    }
}

/// Runs one `LspNavigate` call headless in `mode`; returns `(is_error, text)`.
/// The op is unknown, so a dispatched call fails in the tool's own argument
/// check, before it would start a server.
async fn navigate(mode: PermissionMode) -> (bool, String) {
    let ws = tempfile::tempdir().expect("ws");
    let call = Block::ToolUse {
        id: "n".into(),
        name: "LspNavigate".into(),
        input_json: serde_json::to_vec(&serde_json::json!({
            "op": "no_such_op", "path": "src/lib.rs", "line": 1, "col": 1
        }))
        .expect("json"),
        cache_marker: None,
    };
    let turns = [call, Block::text("done")]
        .into_iter()
        .map(|b| ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(b),
            usage: Usage::default(),
        })
        .collect();
    let opts = LoopOptions {
        cwd: Some(ws.path().to_path_buf()),
        permission_mode: mode,
        ..LoopOptions::default().without_streaming()
    };
    let mut session = Session::new("scripted", "m");
    run_loop(
        &mut session,
        "go",
        &ScriptedProvider(Mutex::new(turns)),
        &NoPrompter,
        &opts,
    )
    .await
    .expect("run");
    session
        .messages
        .iter()
        .flat_map(|m| &m.blocks)
        .find_map(|b| match b {
            Block::ToolResult {
                tool_use_id,
                is_error,
                inline,
                ..
            } if tool_use_id == "n" => Some((
                *is_error,
                String::from_utf8_lossy(inline.as_deref().unwrap_or_default()).into_owned(),
            )),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no result"))
}

#[tokio::test]
async fn lsp_navigate_is_denied_where_nobody_can_ask_and_dispatched_in_mode_off() {
    let (err, text) = navigate(PermissionMode::Workspace).await;
    assert!(err && text.contains("cannot ask"), "{text}");
    assert!(
        !text.contains("unknown lsp navigate op"),
        "never dispatched: {text}"
    );
    let (err, text) = navigate(PermissionMode::Off).await;
    assert!(
        err && text.contains("unknown lsp navigate op"),
        "dispatched: {text}"
    );
}

#[test]
fn lsp_tools_require_permission() {
    for name in ["LspNavigate", "Diagnostics"] {
        let meta = origin_tools::registry_iter()
            .find(|m| m.name == name)
            .unwrap_or_else(|| panic!("{name} registered"));
        assert_eq!(meta.tier, origin_tools::Tier::RequiresPermission, "{name}");
    }
}
