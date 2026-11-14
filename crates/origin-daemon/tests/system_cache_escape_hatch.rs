// SPDX-License-Identifier: Apache-2.0
//! B2 escape hatch on the LIVE path: the daemon's rolling message markers and
//! the Anthropic encoder's `system` marker must agree on `ORIGIN_SYSTEM_CACHE`.
//!
//! Default: `system` is a marked block, so the messages get 3 rolling markers.
//! `ORIGIN_SYSTEM_CACHE=0`: `system` is a plain string, so the messages get all
//! 4. Either way the encoded request never exceeds Anthropic's 4.
//!
//! Its own test binary: it mutates a process-wide env var, and both states run
//! sequentially in one test so nothing else observes the toggle.

#![allow(clippy::panic, clippy::unwrap_used)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

const TOOL_TURNS: u32 = 6;

/// Issues `TOOL_TURNS` `Read` calls on distinct files (identical repeats would
/// trip the stuck-loop detector), then answers with text.
struct ToolTurns {
    turn: AtomicU32,
    dir: std::path::PathBuf,
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl Provider for ToolTurns {
    fn name(&self) -> &'static str {
        "tool-turns"
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.requests.lock().unwrap().push(req);
        let t = self.turn.fetch_add(1, Ordering::SeqCst);
        let block = if t < TOOL_TURNS {
            Block::ToolUse {
                id: format!("call-{t}"),
                name: "Read".into(),
                input_json: serde_json::to_vec(
                    &serde_json::json!({ "file_path": self.dir.join(format!("note{t}.txt")) }),
                )
                .unwrap(),
                cache_marker: None,
            }
        } else {
            Block::text("done")
        };
        Ok(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(block),
            usage: Usage::default(),
        })
    }
}

/// Encode the session's final request (after `TOOL_TURNS` tool rounds) as the
/// Anthropic provider would. Returns (system, message-marker count, total).
async fn final_request(dir: &std::path::Path) -> (serde_json::Value, usize, usize) {
    let provider = ToolTurns {
        turn: AtomicU32::new(0),
        dir: dir.to_path_buf(),
        requests: Mutex::new(Vec::new()),
    };
    let mut session = Session::new("tool-turns", "claude-test");
    let opts = LoopOptions::default().without_streaming();
    run_loop(&mut session, "read it repeatedly", &provider, &AlwaysAllow, &opts)
        .await
        .unwrap();
    let last = provider.requests.lock().unwrap().last().cloned().unwrap();
    let body = origin_provider_anthropic::encode_request_for_test(&last);
    let count = |v: &serde_json::Value| v.to_string().matches("\"cache_control\"").count();
    (body["system"].clone(), count(&body["messages"]), count(&body))
}

#[tokio::test]
async fn rolling_message_markers_follow_the_system_cache_flag() {
    let dir = tempfile::tempdir().unwrap();
    for t in 0..TOOL_TURNS {
        std::fs::write(dir.path().join(format!("note{t}.txt")), format!("note {t}")).unwrap();
    }

    std::env::remove_var("ORIGIN_SYSTEM_CACHE");
    let (system, messages, total) = final_request(dir.path()).await;
    assert!(system.is_array(), "default: marked system block");
    assert_eq!(messages, 3, "default: three rolling message markers");
    assert_eq!(total, 4);

    std::env::set_var("ORIGIN_SYSTEM_CACHE", "0");
    let (system, messages, total) = final_request(dir.path()).await;
    std::env::remove_var("ORIGIN_SYSTEM_CACHE");
    assert!(system.is_string(), "escape hatch: plain-string system");
    assert_eq!(messages, 4, "escape hatch: the messages get all four markers");
    assert_eq!(total, 4);
}
