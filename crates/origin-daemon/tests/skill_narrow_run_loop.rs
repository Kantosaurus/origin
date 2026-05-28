//! End-to-end wiring check: an active `SkillRegistry` on `LoopOptions` must
//! cause `run_loop` to deny a tool that falls outside the intersected
//! `allowed-tools` mask.
//!
//! Pairs the existing permission-layer test in `origin-permission/tests/
//! skill_narrow.rs`, but exercises the daemon's actual dispatch path so a
//! regression that forgets to wire `LoopOptions.skills` into the per-turn
//! check would be caught here.

#![allow(clippy::panic)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopError, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use origin_skills::frontmatter::SkillFrontmatter;
use origin_skills::SkillRegistry;
use std::sync::{Arc, Mutex};

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

#[tokio::test]
async fn skill_narrowing_denies_tool_in_run_loop() {
    // Skill stack only permits `Read`. The scripted assistant tries to call
    // `Bash`, which is outside the mask → run_loop must return Denied.
    let mut skills = SkillRegistry::new();
    skills.activate(SkillFrontmatter {
        name: "read-only".into(),
        description: "limits the agent to Read".into(),
        allowed_tools: vec!["Read".into()],
    });

    let tool_use = Block::ToolUse {
        id: "tu_1".into(),
        name: "Bash".into(),
        input_json: serde_json::to_vec(&serde_json::json!({"command": "echo hi"})).expect("json"),
        cache_marker: None,
    };
    let provider = ScriptedProvider {
        responses: Mutex::new(vec![ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(tool_use),
            usage: Usage::default(),
        }]),
    };

    let mut session = Session::new("skill-narrow", "scripted-model");
    let opts = LoopOptions::default()
        .without_streaming()
        .with_skills(Arc::new(skills));

    let err = run_loop(&mut session, "go", &provider, &AlwaysAllow, &opts)
        .await
        .expect_err("run_loop must deny Bash under read-only skill");
    match err {
        LoopError::Denied(name) => assert_eq!(name, "Bash"),
        other => panic!("expected Denied(Bash), got {other:?}"),
    }
}

#[tokio::test]
async fn no_skills_option_preserves_default_behavior() {
    // Sanity: with `skills: None`, an `AutoAllowed` tool (Read) goes through.
    let path = std::env::temp_dir().join(format!("origin-skill-narrow-{}.txt", ulid::Ulid::new()));
    std::fs::write(&path, "ok").expect("write tempfile");
    let path_str = path.to_str().expect("utf8 path").to_string();

    let tool_use = Block::ToolUse {
        id: "tu_1".into(),
        name: "Read".into(),
        input_json: serde_json::to_vec(&serde_json::json!({"path": path_str.clone()})).expect("json"),
        cache_marker: None,
    };
    let final_text = Block::text("done");
    let provider = ScriptedProvider {
        responses: Mutex::new(vec![
            ChatResponse {
                assistant: Message::new(Role::Assistant).with_block(tool_use),
                usage: Usage::default(),
            },
            ChatResponse {
                assistant: Message::new(Role::Assistant).with_block(final_text),
                usage: Usage::default(),
            },
        ]),
    };

    let mut session = Session::new("skill-narrow", "scripted-model");
    let summary = run_loop(
        &mut session,
        "read",
        &provider,
        &AlwaysAllow,
        &LoopOptions::default().without_streaming(),
    )
    .await
    .expect("run_loop ok");

    let _ = std::fs::remove_file(&path);
    assert_eq!(summary.assistant_text, "done");
    assert_eq!(summary.turns, 2);
}
