// SPDX-License-Identifier: Apache-2.0
//! D3: `ORIGIN_SWARM_ALWAYS=1` restores the "swarm is always on" and mandatory
//! verifier-`Task` mandates. The variable is read once per process, so this
//! test owns its own binary and sets it before anything reads it.

#![allow(clippy::unwrap_used)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::Mutex;

struct Recording(Mutex<Vec<String>>);

#[async_trait]
impl Provider for Recording {
    fn name(&self) -> &'static str {
        "recording"
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.0.lock().unwrap().push(req.system);
        Ok(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
            usage: Usage::default(),
        })
    }
}

#[tokio::test]
async fn swarm_always_restores_both_mandates() {
    std::env::set_var("ORIGIN_SWARM_ALWAYS", "1");
    let provider = Recording(Mutex::new(Vec::new()));
    let mut session = Session::new("recording", "claude-test");
    let opts = LoopOptions {
        max_turns: 1,
        ..LoopOptions::default().without_streaming()
    };
    run_loop(&mut session, "hi", &provider, &AlwaysAllow, &opts)
        .await
        .unwrap();
    let system = provider.0.lock().unwrap().remove(0);
    assert!(system.contains("SWARM IS ALWAYS ON"), "{system}");
    assert!(system.contains("dispatch a SEPARATE Task"), "{system}");
}
