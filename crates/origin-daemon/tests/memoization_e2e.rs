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
use origin_stream::{Ring, TokenEvent, TokenKind};
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

/// Plays `steps` as one tool call per turn, then "done". `on_turn` runs at the
/// start of each `chat` (before the step for that turn is returned).
struct StepProvider {
    turn: AtomicU32,
    steps: Vec<(&'static str, serde_json::Value)>,
    on_turn: Box<dyn Fn(u32) + Send + Sync>,
}

#[async_trait]
impl Provider for StepProvider {
    fn name(&self) -> &'static str {
        "scripted"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let t = self.turn.fetch_add(1, Ordering::SeqCst);
        (self.on_turn)(t);
        let blocks = match self.steps.get(t as usize) {
            Some((name, input)) => vec![Block::ToolUse {
                id: format!("id-{t}"),
                name: (*name).into(),
                input_json: serde_json::to_vec(input).expect("json"),
                cache_marker: None,
            }],
            None => vec![Block::Text {
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

fn open_store(dir: &std::path::Path) -> Arc<Store> {
    Arc::new(
        Store::open(StoreConfig {
            root: dir.join("cas"),
            hot_capacity: 64,
            warm_pack_target_bytes: 64 * 1024,
            cold_zstd_level: 3,
        })
        .expect("open"),
    )
}

/// Every tool-result body in the session, in order (CAS handle or inline).
fn tool_result_bodies(session: &Session, store: &Store) -> Vec<String> {
    session
        .snapshot()
        .iter()
        .filter(|m| matches!(m.role, Role::Tool))
        .flat_map(|m| m.blocks.clone())
        .map(|b| match b {
            Block::ToolResult { handle: Some(h), .. } => String::from_utf8(
                store
                    .get(origin_cas::Hash::from_bytes(h))
                    .expect("get")
                    .expect("present"),
            )
            .expect("utf8"),
            Block::ToolResult { inline: Some(b), .. } => String::from_utf8(b).expect("utf8"),
            other => panic!("unexpected tool block: {other:?}"),
        })
        .collect()
}

/// A2: Read → Edit (real dispatch) → identical Read must see the edited bytes,
/// not a memoized pre-edit copy.
#[tokio::test]
async fn read_after_edit_returns_new_bytes() {
    let dir = tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let p = dir.path().join("hello.txt");
    std::fs::write(&p, "hello world").expect("write");
    let path = p.to_string_lossy().into_owned();
    let read = serde_json::json!({"file_path": &path});

    let provider = StepProvider {
        turn: AtomicU32::new(0),
        steps: vec![
            ("Read", read.clone()),
            (
                "Edit",
                serde_json::json!({"file_path": &path, "old_string": "hello", "new_string": "goodbye"}),
            ),
            ("Read", read),
        ],
        on_turn: Box::new(|_| {}),
    };
    let mut session = Session::new("test-session", "scripted-model");
    let opts = LoopOptions::default().with_cas(store.clone()).without_streaming();
    let _ = run_loop(&mut session, "go", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");

    assert_eq!(
        std::fs::read_to_string(&p).expect("read"),
        "goodbye world",
        "Edit must have applied"
    );
    let bodies = tool_result_bodies(&session, &store);
    assert_eq!(bodies.len(), 3, "got: {bodies:?}");
    let reread = &bodies[2];
    assert!(
        reread.contains("goodbye world"),
        "re-Read after Edit returned stale bytes: {reread}"
    );
    assert!(
        !reread.contains("(cached from turn"),
        "re-Read after Edit was served from the memo: {reread}"
    );
}

/// A2: Read (miss) → Read (hit) → out-of-band change → Write → Read. The hit
/// skips `note_read`, but the miss already stamped the file, so the write guard
/// still sees the change and refuses the Write; that refused Write then drops the
/// memo, so the model's re-Read gets the real bytes instead of looping on a stale
/// hit. (Stamping on the hit would have let the Write clobber bytes never seen.)
#[tokio::test]
async fn write_guard_sees_external_change_after_cached_read() {
    let dir = tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let p = dir.path().join("hello.txt");
    std::fs::write(&p, "hello world").expect("write");
    let path = p.to_string_lossy().into_owned();
    let read = serde_json::json!({"file_path": &path});
    let guard = Arc::new(origin_tools::builtins::write::WriteGuard::default());

    let (g, ext) = (guard.clone(), p.clone());
    let provider = StepProvider {
        turn: AtomicU32::new(0),
        steps: vec![
            ("Read", read.clone()),
            ("Read", read.clone()),
            (
                "Write",
                serde_json::json!({"file_path": &path, "content": "model write"}),
            ),
            ("Read", read),
        ],
        on_turn: Box::new(move |t| {
            if t == 2 {
                // Both Reads have run and the file is unchanged: guard counts it read.
                assert!(
                    g.has_read(ext.to_str().expect("utf8")),
                    "guard must count the file as read"
                );
                std::fs::write(&ext, "changed out of band, longer").expect("external write");
            }
        }),
    };
    let mut session = Session::new("test-session", "scripted-model");
    let mut opts = LoopOptions::default().with_cas(store.clone()).without_streaming();
    opts.write_guard = Some(guard.clone());
    let _ = run_loop(&mut session, "go", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");

    let bodies = tool_result_bodies(&session, &store);
    assert_eq!(bodies.len(), 4, "got: {bodies:?}");
    assert!(
        bodies[1].contains("(cached from turn"),
        "second Read must be a memo hit: {}",
        bodies[1]
    );
    assert!(
        bodies[2].contains("refusing to overwrite"),
        "guard must refuse the Write: {}",
        bodies[2]
    );
    assert!(
        bodies[3].contains("changed out of band") && !bodies[3].contains("(cached from turn"),
        "re-Read after the refused Write must be fresh: {}",
        bodies[3]
    );
    assert_eq!(
        std::fs::read_to_string(&p).expect("read"),
        "changed out of band, longer",
        "the out-of-band bytes must survive"
    );
}

/// Streams `turns[t]` as one assistant message of tool calls, then "done".
/// Streaming is what makes the loop pre-run Pure calls (speculative dispatch).
struct StreamStepProvider {
    turn: AtomicU32,
    turns: Vec<Vec<(&'static str, serde_json::Value)>>,
}

#[async_trait]
impl Provider for StreamStepProvider {
    fn name(&self) -> &'static str {
        "scripted-stream"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        Err(ProviderError::Api("non-stream path should not be hit".into()))
    }
    async fn chat_stream(&self, _req: ChatRequest, ring: &Ring) -> Result<(), ProviderError> {
        let t = self.turn.fetch_add(1, Ordering::SeqCst);
        let publish = |kind, payload: Vec<u8>| {
            ring.publish(&TokenEvent::new(kind, payload))
                .map_err(|e| ProviderError::Api(e.to_string()))
        };
        match self.turns.get(t as usize) {
            Some(calls) => {
                for (i, (name, input)) in calls.iter().enumerate() {
                    let idx = u32::try_from(i).expect("index").to_le_bytes();
                    let mut start = idx.to_vec();
                    start.extend_from_slice(format!("id-{t}-{i}\0{name}").as_bytes());
                    publish(TokenKind::ToolUseStart, start)?;
                    let mut delta = idx.to_vec();
                    delta.extend_from_slice(&serde_json::to_vec(input).expect("json"));
                    publish(TokenKind::ToolUseDelta, delta)?;
                }
                // Let any speculative pre-run finish before the turn ends, so it
                // deterministically reads the file before the batch dispatches.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            None => publish(TokenKind::TextDelta, b"done".to_vec())?,
        }
        publish(TokenKind::TurnEnd, Vec::new())?;
        ring.close();
        Ok(())
    }
}

/// A2 fix round 1: with streaming on (the default), `[Edit X, Read X]` in ONE
/// assistant message must not consume the Read pre-run before the Edit, nor let
/// those pre-edit bytes into the memo for a later identical Read.
#[tokio::test]
async fn streamed_edit_then_read_in_one_message_returns_new_bytes() {
    let dir = tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let p = dir.path().join("hello.txt");
    std::fs::write(&p, "hello world").expect("write");
    let path = p.to_string_lossy().into_owned();
    let read = serde_json::json!({"file_path": &path});
    let edit = serde_json::json!({"file_path": &path, "old_string": "hello", "new_string": "goodbye"});

    let provider = StreamStepProvider {
        turn: AtomicU32::new(0),
        turns: vec![vec![("Edit", edit), ("Read", read.clone())], vec![("Read", read)]],
    };
    let mut session = Session::new("test-session", "scripted-model");
    let opts = LoopOptions::default().with_cas(store.clone());
    let _ = run_loop(&mut session, "go", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");

    assert_eq!(
        std::fs::read_to_string(&p).expect("read"),
        "goodbye world",
        "Edit must have applied"
    );
    let bodies = tool_result_bodies(&session, &store);
    assert_eq!(bodies.len(), 3, "got: {bodies:?}");
    assert!(
        bodies[1].contains("goodbye world") && bodies[2].contains("goodbye world"),
        "stale bytes after Edit; same-message Read: {:?}, next-turn Read: {:?}",
        bodies[1],
        bodies[2]
    );
}
