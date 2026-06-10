// SPDX-License-Identifier: Apache-2.0
//! Regression: activating a skill must inject its `SKILL.md` *body* into the
//! per-turn system prompt, not just its one-line catalog description.
//!
//! The bug: `/<name>` activation pushed only the skill's frontmatter onto the
//! active-skill stack, so the `allowed-tools` mask + catalog `*` marker took
//! effect ("the skill loads") but the model never received the skill's actual
//! instructions and so never carried them out. The fix carries the body
//! through activation and surfaces it under `<origin-active-skills>`.

#![allow(clippy::panic, clippy::unwrap_used, clippy::doc_markdown)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use origin_skills::{SkillFrontmatter, SkillRegistry};
use std::sync::{Arc, Mutex};

struct CapturingProvider {
    seen_systems: Mutex<Vec<String>>,
}

#[async_trait]
impl Provider for CapturingProvider {
    fn name(&self) -> &'static str {
        "capturing"
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.seen_systems.lock().expect("lock").push(req.system);
        Ok(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
            usage: Usage::default(),
        })
    }
}

#[tokio::test]
async fn active_skill_body_lands_in_system_prompt() {
    // A skill carrying a distinctive instruction in its body.
    const MARKER: &str = "ALWAYS-EMIT-THE-WIDGET-TAG";
    let mut reg = SkillRegistry::new();
    reg.activate_with_body(
        SkillFrontmatter {
            name: "widget".into(),
            description: "Does widget things".into(),
            allowed_tools: vec!["Read".into()],
        },
        format!("# Widget\n\nYou MUST {MARKER} on every reply."),
    );

    let provider = CapturingProvider {
        seen_systems: Mutex::new(Vec::new()),
    };
    let mut session = Session::new("test", "test-model");
    let opts = LoopOptions {
        max_turns: 1,
        skills: Some(Arc::new(reg)),
        ..LoopOptions::default().without_streaming()
    };
    let _ = run_loop(&mut session, "hi", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");
    let sys = provider.seen_systems.lock().expect("lock").remove(0);
    assert!(
        sys.contains("<origin-active-skills>"),
        "system prompt missing <origin-active-skills> block:\n{sys}"
    );
    assert!(
        sys.contains(MARKER),
        "active skill's SKILL.md body must reach the model:\n{sys}"
    );
    assert!(
        sys.contains("name=\"widget\""),
        "active-skills block must name the skill:\n{sys}"
    );
}

#[tokio::test]
async fn no_active_skills_means_no_active_block() {
    // With no skill registry the block must be absent (byte-stable prompt).
    let provider = CapturingProvider {
        seen_systems: Mutex::new(Vec::new()),
    };
    let mut session = Session::new("test", "test-model");
    let opts = LoopOptions {
        max_turns: 1,
        ..LoopOptions::default().without_streaming()
    };
    let _ = run_loop(&mut session, "hi", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");
    let sys = provider.seen_systems.lock().expect("lock").remove(0);
    assert!(
        !sys.contains("<origin-active-skills>"),
        "no skills active ⇒ no active-skills block:\n{sys}"
    );
}
