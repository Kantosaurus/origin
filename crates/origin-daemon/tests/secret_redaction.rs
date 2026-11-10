// SPDX-License-Identifier: Apache-2.0
//! C9: a tool result echoing a configured key value reaches the model, the CAS
//! and the saved session as `[REDACTED:<NAME>]`, never the raw value. Own test
//! binary because the redactor is process-wide (installed once).
#![allow(clippy::panic, clippy::unwrap_used)]

use async_trait::async_trait;
use origin_cas::{Store, StoreConfig};
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::redact::{install, Redactor};
use origin_daemon::session::Session;
use origin_daemon::session_store::SessionStore;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::{Arc, Mutex};

const KEY: &str = "sk-ant-c9-test-value-0123456789";
const MARKER: &str = "[REDACTED:ANTHROPIC_API_KEY]";

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

fn tool_result_text(msgs: &[Message]) -> String {
    msgs.iter()
        .flat_map(|m| &m.blocks)
        .filter_map(|b| match b {
            Block::ToolResult { inline: Some(v), .. } => Some(String::from_utf8_lossy(v).into_owned()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn tool_result_echoing_a_key_is_redacted_for_model_and_saved_session() {
    install(Redactor::new([(
        "ANTHROPIC_API_KEY".to_string(),
        KEY.to_string(),
    )]));

    let dir = tempfile::tempdir().unwrap();
    let cas = Arc::new(
        Store::open(StoreConfig {
            root: dir.path().join("cas"),
            hot_capacity: 32,
            warm_pack_target_bytes: 1024 * 1024,
            cold_zstd_level: 3,
        })
        .unwrap(),
    );
    let file = dir.path().join("dotenv.txt");
    std::fs::write(&file, format!("ANTHROPIC_API_KEY={KEY}\nOTHER=short12\n")).unwrap();

    let tool_use = Block::ToolUse {
        id: "tu_1".into(),
        name: "Read".into(),
        input_json: serde_json::to_vec(&serde_json::json!({"file_path": file.to_str().unwrap()})).unwrap(),
        cache_marker: None,
    };
    let provider = Recording {
        responses: Mutex::new(vec![
            ChatResponse {
                assistant: Message::new(Role::Assistant).with_block(tool_use),
                usage: Usage::default(),
            },
            ChatResponse {
                assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
                usage: Usage::default(),
            },
        ]),
        seen: Mutex::new(Vec::new()),
    };
    let opts = LoopOptions {
        cas: Some(Arc::clone(&cas)),
        streaming_disabled: true,
        ..LoopOptions::default()
    };
    let mut session = Session::new("test", "claude-opus-4-7");
    run_loop(
        &mut session,
        &format!("read it; fyi my key is {KEY}"),
        &provider,
        &AlwaysAllow,
        &opts,
    )
    .await
    .expect("loop ok");

    // Model-facing: the second request, inflated exactly as every provider does.
    let second = provider.seen.lock().unwrap()[1].clone();
    let wire = origin_provider::inflate_tool_result_handles(&second, Some(&cas)).unwrap();
    let body = tool_result_text(&wire);
    assert!(body.contains(MARKER), "model-facing tool result: {body}");
    assert!(!body.contains(KEY), "raw key reached the model: {body}");
    assert!(body.contains("short12"), "non-secret text is untouched: {body}");

    // Saved session: persisted through the store, reloaded, inflated from CAS.
    let store = SessionStore::open(dir.path().join("sessions.db")).unwrap();
    store.persist_session(&session).unwrap();
    store.persist_transcript(&session.id, &session.messages).unwrap();
    let loaded = store.load_messages(&session.id).unwrap();
    let saved = origin_provider::inflate_tool_result_handles(&loaded, Some(&cas)).unwrap();
    let dump = format!("{saved:?}{}", tool_result_text(&saved));
    assert!(dump.contains(MARKER), "saved session: {dump}");
    assert!(!dump.contains(KEY), "raw key persisted: {dump}");
    // The session title is derived from that first user line.
    let title = store.list_summaries().unwrap()[0].title.clone().unwrap();
    assert!(title.contains(MARKER) && !title.contains(KEY), "title: {title}");
}
