// SPDX-License-Identifier: Apache-2.0
//! `Read` the same path twice in one session — second call must be served
//! from the cache and the result body must contain "(cached from turn ".
#![allow(clippy::panic)]
#![allow(clippy::indexing_slicing)]

use async_trait::async_trait;
use origin_cas::{Store, StoreConfig};
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tempfile::tempdir;

struct ScriptedProvider {
    turn: AtomicU32,
    target_path: String,
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn name(&self) -> &'static str {
        "scripted"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let t = self.turn.fetch_add(1, Ordering::SeqCst);
        let blocks = match t {
            0 | 1 => vec![Block::ToolUse {
                id: format!("id-{t}"),
                name: "Read".into(),
                input_json: serde_json::to_vec(&serde_json::json!({"file_path": &self.target_path}))
                    .expect("json"),
                cache_marker: None,
            }],
            _ => vec![Block::Text {
                text: "done".into(),
                cache_marker: None,
            }],
        };
        Ok(ChatResponse {
            assistant: Message {
                role: Role::Assistant,
                blocks,
            },
            usage: Usage::default(),
        })
    }
}

#[tokio::test]
async fn second_read_serves_from_cache_with_annotation() {
    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        Store::open(StoreConfig {
            root: dir.path().to_path_buf(),
            hot_capacity: 64,
            warm_pack_target_bytes: 64 * 1024,
            cold_zstd_level: 3,
        })
        .expect("open"),
    );
    let p = dir.path().join("hello.txt");
    std::fs::write(&p, "hello world").expect("write");

    let provider = ScriptedProvider {
        turn: AtomicU32::new(0),
        target_path: p.to_string_lossy().into_owned(),
    };

    let mut session = Session::new("test-session", "scripted-model");
    let opts = LoopOptions::default().with_cas(store.clone()).without_streaming();
    let _ = run_loop(&mut session, "go", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");

    // snapshot() returns owned Messages; keep a binding so references live long enough.
    let snapshot = session.snapshot();
    let tool_results: Vec<&Block> = snapshot
        .iter()
        .filter(|m| matches!(m.role, Role::Tool))
        .flat_map(|m| m.blocks.iter())
        .collect();
    assert!(
        tool_results.len() >= 2,
        "expected ≥ 2 tool results, got {}",
        tool_results.len()
    );

    let second = match tool_results.get(1) {
        Some(Block::ToolResult { handle: Some(h), .. }) => store
            .get(origin_cas::Hash::from_bytes(*h))
            .expect("get")
            .expect("present"),
        other => panic!("second tool result missing handle, got: {other:?}"),
    };
    let txt = String::from_utf8(second).expect("utf8");
    assert!(txt.contains("(cached from turn"), "got: {txt}");
}
