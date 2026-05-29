// SPDX-License-Identifier: Apache-2.0
//! Confirms the agent's system prompt includes a one-liner per skill in
//! the daemon's catalog.

#![allow(clippy::panic)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_daemon::skill_catalog::SkillCatalog;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::{Arc, Mutex};

/// Capture the `system` field of every `ChatRequest` the `run_loop` emits, so
/// the test can assert the catalog text was injected.
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
        // Reply with a terminal turn so the loop exits immediately.
        Ok(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
            usage: Usage::default(),
        })
    }
}

fn write_skill(dir: &std::path::Path, name: &str, desc: &str) {
    let skill_dir = dir.join(name);
    std::fs::create_dir_all(&skill_dir).expect("mkdir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {desc}\nallowed-tools: [\"Read\"]\n---\nbody\n"),
    )
    .expect("write");
}

#[tokio::test]
async fn system_prompt_lists_each_skill_in_catalog() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_skill(dir.path(), "alpha", "Does alpha things");
    write_skill(dir.path(), "beta", "Does beta things");
    let catalog = Arc::new(SkillCatalog::load_from(dir.path()).expect("load"));

    let provider = CapturingProvider {
        seen_systems: Mutex::new(Vec::new()),
    };
    let mut session = Session::new("test", "test-model");
    let opts = LoopOptions {
        max_turns: 1,
        skill_catalog: Some(Arc::clone(&catalog)),
        ..LoopOptions::default().without_streaming()
    };
    let _ = run_loop(&mut session, "hi", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");

    let systems = provider.seen_systems.lock().expect("lock").clone();
    assert_eq!(systems.len(), 1);
    let sys = &systems[0];
    assert!(sys.contains("alpha"), "system prompt missing alpha:\n{sys}");
    assert!(
        sys.contains("Does alpha things"),
        "alpha description missing:\n{sys}"
    );
    assert!(sys.contains("beta"), "system prompt missing beta:\n{sys}");
}

#[tokio::test]
async fn empty_catalog_does_not_pollute_system_prompt() {
    // `SkillCatalog::default()` is the truly-empty catalog. `load_from` on
    // an empty dir still returns the embedded superpowers skills, which is
    // intentional but not what this test wants to exercise.
    let catalog = Arc::new(SkillCatalog::default());
    let provider = CapturingProvider {
        seen_systems: Mutex::new(Vec::new()),
    };
    let mut session = Session::new("test", "test-model");
    let opts = LoopOptions {
        max_turns: 1,
        skill_catalog: Some(Arc::clone(&catalog)),
        ..LoopOptions::default().without_streaming()
    };
    let _ = run_loop(&mut session, "hi", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");
    let systems = provider.seen_systems.lock().expect("lock").clone();
    // Empty catalog → empty (or unchanged-by-injection) system prompt.
    assert!(
        !systems[0].contains("Available skills"),
        "empty catalog should not emit `Available skills` header"
    );
}
