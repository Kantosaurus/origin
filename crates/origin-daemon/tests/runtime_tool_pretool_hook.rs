// SPDX-License-Identifier: Apache-2.0
//! A4: the `PreTool` hook fires for a sub-agent's inline-MCP ("runtime") tool
//! and its `Deny` is honoured like a registry tool's. Own test binary: the hooks
//! runtime is built once per process from `$ORIGIN_HOME/.origin/hooks.json`.
#![allow(clippy::panic, clippy::unwrap_used)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, scope_runtime_tools, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use origin_tools::{DynTool, SandboxProfile, SideEffects, Tier, ToolMeta, Urgency};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

struct Scripted {
    script: Mutex<Vec<ChatResponse>>,
    seen: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl Provider for Scripted {
    fn name(&self) -> &'static str {
        "scripted"
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.seen.lock().unwrap().push(req);
        let mut q = self.script.lock().unwrap();
        if q.is_empty() {
            return Err(ProviderError::Api("script exhausted".into()));
        }
        Ok(q.remove(0))
    }
}

#[derive(Debug)]
struct Counted(ToolMeta, Arc<AtomicUsize>);

#[async_trait]
impl DynTool for Counted {
    fn meta(&self) -> &ToolMeta {
        &self.0
    }
    async fn invoke(&self, _args: Value) -> Result<Value, String> {
        self.1.fetch_add(1, Ordering::SeqCst);
        Ok(Value::String("ran".into()))
    }
}

/// A hook that answers every `PreTool` event with a `Deny`.
fn write_deny_hook(home: &std::path::Path) {
    let deny = r#"{"override":{"action":"deny","reason":"hooked"}}"#;
    let (program, args) = if cfg!(windows) {
        (
            "powershell",
            vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                format!(
                    "while ($null -ne ($l = [Console]::In.ReadLine())) {{ [Console]::Out.Write('{deny}' + [char]0); [Console]::Out.Flush() }}"
                ),
            ],
        )
    } else {
        (
            "sh",
            vec![
                "-c".to_string(),
                format!("while IFS= read -r l; do printf '%s\\0' '{deny}'; done"),
            ],
        )
    };
    let dir = home.join(".origin");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = serde_json::json!({ "hooks": [{ "event": "pre_tool", "program": program, "args": args }] });
    std::fs::write(dir.join("hooks.json"), cfg.to_string()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_pretool_hook_fires_for_a_runtime_tool_and_its_deny_holds() {
    let home = tempfile::TempDir::new().unwrap();
    write_deny_hook(home.path());
    std::env::set_var("ORIGIN_HOME", home.path());

    let calls = Arc::new(AtomicUsize::new(0));
    let mut tools: HashMap<String, Arc<dyn DynTool>> = HashMap::new();
    tools.insert(
        "mcp__fake__op".into(),
        Arc::new(Counted(
            ToolMeta {
                name: "mcp__fake__op",
                description: "fake",
                tier: Tier::AutoAllowed,
                urgency: Urgency::Low,
                side_effects: SideEffects::Pure,
                input_schema: r#"{"type":"object"}"#,
                sandbox_profile: SandboxProfile::Inherit,
                hot: false,
            },
            Arc::clone(&calls),
        )),
    );
    let provider = Scripted {
        script: Mutex::new(vec![
            ChatResponse {
                assistant: Message::new(Role::Assistant).with_block(Block::ToolUse {
                    id: "T1".into(),
                    name: "mcp__fake__op".into(),
                    input_json: b"{}".to_vec(),
                    cache_marker: None,
                }),
                usage: Usage::default(),
            },
            ChatResponse {
                assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
                usage: Usage::default(),
            },
        ]),
        seen: Mutex::new(Vec::new()),
    };
    let mut session = Session::new_with_id("sess-a4-hook".into(), "test-model".into());
    let opts = LoopOptions::default().without_streaming();
    Box::pin(scope_runtime_tools(Arc::new(tools), async {
        Box::pin(run_loop(&mut session, "go", &provider, &AlwaysAllow, &opts))
            .await
            .unwrap();
    }))
    .await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a hook-denied tool is never invoked"
    );
    let body = session
        .messages
        .iter()
        .flat_map(|m| &m.blocks)
        .find_map(|b| match b {
            Block::ToolResult {
                inline: Some(body),
                is_error: true,
                ..
            } => Some(String::from_utf8_lossy(body).into_owned()),
            _ => None,
        })
        .expect("an error result");
    assert!(
        body.starts_with("Error: permission denied for `mcp__fake__op` (tier=")
            && body.contains("hook denied: hooked"),
        "{body}"
    );
}
