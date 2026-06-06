// SPDX-License-Identifier: Apache-2.0
//! Rewind-across-compaction wiring contract.
//!
//! When the per-request loop carries a [`SessionStore`] (the production wire,
//! `LoopOptions.session_store = Some(..)`), the in-loop compactor (a) loads the
//! per-turn summaries and folds the oldest summarized turns once the transcript
//! crosses the soft cap, and (b) snapshots each folded turn's pre-compaction
//! body so a later `origin sessions rewind` can reconstruct it.
//!
//! With `session_store = None` (the pre-fix production state) the compactor can
//! never load a summary, so it never folds and never snapshots — `rewind`
//! degrades to a plain truncate. These tests pin both halves so the wiring
//! cannot silently regress back to `None`.
#![allow(clippy::panic)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_daemon::session_store::SessionStore;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

struct ScriptedProvider {
    responses: Mutex<Vec<ChatResponse>>,
}

impl ScriptedProvider {
    #[allow(clippy::missing_const_for_fn)] // Mutex::new is not const-stable
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }
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

fn first_text(m: &Message) -> String {
    match m.blocks.first() {
        Some(Block::Text { text, .. }) => text.clone(),
        _ => String::new(),
    }
}

const SID: &str = "sess-compact";

/// Seed a store + an over-cap in-memory session: the oldest four turns are
/// small and summarized (foldable), and one large recent turn pushes the
/// transcript past the 200 KiB soft cap. Returns the live store so callers can
/// assert on snapshots after the loop.
fn seed(dir: &TempDir) -> (Arc<SessionStore>, Session) {
    let store = Arc::new(SessionStore::open(dir.path().join("sessions.db")).expect("open store"));
    store
        .persist_session(&Session::new_with_id(SID.to_string(), "test-model".to_string()))
        .expect("persist session");

    let mut session = Session::new_with_id(SID.to_string(), "test-model".to_string());
    for i in 0..4u32 {
        let body = format!("original turn {i}");
        let m = Message::new(Role::User).with_block(Block::text(body));
        session.messages.push(m.clone());
        store.persist_message(SID, i, &m).expect("persist msg");
        store
            .update_summary(SID, i, &format!("sum-{i}"))
            .expect("summary");
    }
    // A large recent turn pushes the accumulated transcript over the soft cap so
    // the post-turn compaction call actually fires.
    let big = "y".repeat(210_000);
    session
        .messages
        .push(Message::new(Role::User).with_block(Block::text(big)));
    (store, session)
}

/// One tool-using turn (so the loop reaches the post-turn compaction call),
/// followed by a terminating text turn.
fn scripted(tool_path: &str) -> ScriptedProvider {
    ScriptedProvider::new(vec![
        ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::ToolUse {
                id: "tu".into(),
                name: "Read".into(),
                input_json: serde_json::to_vec(&serde_json::json!({ "file_path": tool_path })).expect("json"),
                cache_marker: None,
            }),
            usage: Usage::default(),
        },
        ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
            usage: Usage::default(),
        },
    ])
}

#[tokio::test]
async fn compaction_folds_and_snapshots_when_store_is_wired() {
    let dir = TempDir::new().expect("tempdir");
    let tool = dir.path().join("tool.txt");
    std::fs::write(&tool, "x").expect("write tool file");
    let tool_str = tool.to_str().expect("utf8").to_string();

    let (store, mut session) = seed(&dir);
    let provider = scripted(&tool_str);
    let opts = LoopOptions {
        session_store: Some(Arc::clone(&store)),
        ..LoopOptions::default()
    }
    .without_streaming();

    run_loop(&mut session, "read the file", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");

    // (a) The oldest summarized turns were folded in-memory: summaries load only
    //     when the store is wired, so a fold proves the store was consulted.
    assert!(
        first_text(&session.messages[0]).starts_with("[compacted turn 0]"),
        "oldest turn should be folded into its summary, got: {:?}",
        first_text(&session.messages[0])
    );

    // (b) A pre-compaction snapshot was written, so rewind can reconstruct the
    //     original body. Simulate the persisted transcript going compacted, then
    //     rewind_restoring must bring turn 0 back from its snapshot.
    let placeholder = Message::new(Role::User).with_block(Block::text("[compacted turn 0] sum-0"));
    store
        .persist_message(SID, 0, &placeholder)
        .expect("overwrite with compacted body");
    assert!(first_text(&store.load_messages(SID).expect("load")[0]).starts_with("[compacted turn 0]"));
    store.rewind_restoring(SID, 4).expect("rewind");
    assert_eq!(
        first_text(&store.load_messages(SID).expect("load")[0]),
        "original turn 0",
        "rewind must restore the pre-compaction body from the snapshot the loop wrote"
    );
}

#[tokio::test]
async fn compaction_does_not_fold_without_a_store() {
    let dir = TempDir::new().expect("tempdir");
    let tool = dir.path().join("tool.txt");
    std::fs::write(&tool, "x").expect("write tool file");
    let tool_str = tool.to_str().expect("utf8").to_string();

    let (_store, mut session) = seed(&dir);
    let provider = scripted(&tool_str);
    // The pre-fix production state: no session store on the loop options.
    let opts = LoopOptions::default().without_streaming();

    run_loop(&mut session, "read the file", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");

    assert_eq!(
        first_text(&session.messages[0]),
        "original turn 0",
        "without a wired store the compactor cannot load summaries, so nothing folds"
    );
}
