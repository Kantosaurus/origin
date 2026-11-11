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
/// Installed too, but under the 8-char floor ⇒ never redacted.
const SHORT: &str = "short12";

/// Every test installs the same redactor; first call wins.
fn setup() {
    install(Redactor::new([
        ("ANTHROPIC_API_KEY".to_string(), KEY.to_string()),
        ("SHORT_TOKEN".to_string(), SHORT.to_string()),
    ]));
}

struct Recording {
    responses: Mutex<Vec<ChatResponse>>,
    seen: Mutex<Vec<Vec<Message>>>,
}

impl Recording {
    fn new(blocks: Vec<Block>) -> Self {
        let responses = blocks
            .into_iter()
            .map(|b| ChatResponse {
                assistant: Message::new(Role::Assistant).with_block(b),
                usage: Usage::default(),
            })
            .collect();
        Self {
            responses: Mutex::new(responses),
            seen: Mutex::new(Vec::new()),
        }
    }
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

fn tool_use(name: &str, args: &serde_json::Value) -> Block {
    Block::ToolUse {
        id: format!("tu_{}", ulid::Ulid::new()),
        name: name.into(),
        input_json: serde_json::to_vec(args).unwrap(),
        cache_marker: None,
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
    setup();
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
    std::fs::write(&file, format!("ANTHROPIC_API_KEY={KEY}\nSHORT_TOKEN={SHORT}\n")).unwrap();

    let provider = Recording::new(vec![
        tool_use("Read", &serde_json::json!({"file_path": file.to_str().unwrap()})),
        Block::text("done"),
    ]);
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
    assert!(
        body.contains(&format!("SHORT_TOKEN={SHORT}")),
        "an installed value under 8 chars is not redacted: {body}"
    );

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

async fn run_one_tool(name: &str, args: &serde_json::Value) -> (bool, String) {
    let provider = Recording::new(vec![tool_use(name, args), Block::text("done")]);
    let opts = LoopOptions {
        streaming_disabled: true,
        ..LoopOptions::default()
    };
    let mut session = Session::new("test", "claude-opus-4-7");
    run_loop(&mut session, "edit", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");
    let is_error = session
        .messages
        .iter()
        .flat_map(|m| &m.blocks)
        .any(|b| matches!(b, Block::ToolResult { is_error: true, .. }));
    (is_error, tool_result_text(&session.messages))
}

/// Finding 2: writing an installed marker would replace the user's real secret
/// on disk; the shared dispatch path refuses it. Other bracketed text is fine.
#[tokio::test]
async fn file_writes_carrying_an_installed_marker_are_refused() {
    setup();
    let dir = tempfile::tempdir().unwrap();
    let env_file = dir.path().join(".env");
    let original = format!("ANTHROPIC_API_KEY={KEY}\n");
    std::fs::write(&env_file, &original).unwrap();
    let path = env_file.to_str().unwrap();

    let (err, body) = run_one_tool(
        "Write",
        &serde_json::json!({"file_path": path, "content": format!("ANTHROPIC_API_KEY={MARKER}\nX=1\n"), "force": true}),
    )
    .await;
    assert!(err && body.contains("redacted"), "Write refused: {body}");
    assert_eq!(
        std::fs::read_to_string(&env_file).unwrap(),
        original,
        "file unchanged"
    );

    let (err, body) = run_one_tool(
        "Edit",
        &serde_json::json!({"file_path": path, "old_string": "ANTHROPIC_API_KEY=", "new_string": format!("ANTHROPIC_API_KEY={MARKER} #")}),
    )
    .await;
    assert!(err && body.contains("redacted"), "Edit refused: {body}");
    assert_eq!(
        std::fs::read_to_string(&env_file).unwrap(),
        original,
        "file unchanged"
    );

    let other = dir.path().join("notes.md");
    let (err, body) = run_one_tool(
        "Write",
        &serde_json::json!({"file_path": other.to_str().unwrap(), "content": "see [REDACTED:SOMETHING_ELSE]\n"}),
    )
    .await;
    assert!(!err, "an uninstalled marker is ordinary text: {body}");
    assert!(std::fs::read_to_string(&other)
        .unwrap()
        .contains("[REDACTED:SOMETHING_ELSE]"));
}

/// C9: a recalled memory carrying a key value rides in the prompt's user
/// message (the per-prompt volatile context, next to the repo-map focus
/// block); the provider never sees the value.
#[tokio::test]
async fn a_recalled_memory_carrying_a_key_is_redacted_for_the_model() {
    use origin_mem::EMBED_DIM;
    setup();
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
    let sql = Arc::new(origin_store::Store::open(dir.path().join("origin.db")).unwrap());
    let store = Arc::new(origin_mem::MemoryStore::new(sql, Arc::clone(&cas)));
    // The stub model embeds every text along the all-ones direction; with
    // this sibling tokenizer (every word id 1) that direction is non-zero.
    let model = dir.path().join("stub.onnx");
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../origin-mem/tests/fixtures/stub_minilm.onnx"
        ),
        &model,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("stub.tokenizer.json"),
        r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,
            "pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,
            "model":{"type":"WordLevel","vocab":{"[UNK]":1},"unk_token":"[UNK]"}}"#,
    )
    .unwrap();
    let embedder = Arc::new(origin_mem::Embedder::from_path(&model).unwrap());
    // The memory, indexed slightly off that direction: an exactly parallel
    // pair has a float dot just over 1, which the HNSW metric rejects.
    let training: Vec<[f32; EMBED_DIM]> = (0..origin_mem::NUM_CENTROIDS)
        .map(|i| {
            let mut v = [0_f32; EMBED_DIM];
            v[i % EMBED_DIM] = 1.0;
            v
        })
        .collect();
    store
        .install_quantizer(&origin_mem::Quantizer::fit(&training, 0).unwrap())
        .unwrap();
    let mut vec = [1_f32; EMBED_DIM];
    vec[0] = 2.0;
    let norm = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    for x in &mut vec {
        *x /= norm;
    }
    let id = store.save(&format!("deploy key is {KEY}"), &vec, &[]).unwrap();
    let index = Arc::new(parking_lot::RwLock::new(origin_mem::MemIndex::new()));
    index
        .write()
        .insert(origin_mem::memory_id_to_u64(&id), &vec)
        .unwrap();
    let memory = origin_daemon::memory_wiring::MemoryWiring::new(store, Some(embedder), index);

    let provider = Recording::new(vec![Block::text("done")]);
    let opts = LoopOptions {
        streaming_disabled: true,
        injector: memory.injector.clone(),
        ..LoopOptions::default()
    };
    let mut session = Session::new("test", "claude-opus-4-7");
    run_loop(
        &mut session,
        "what is the deploy key",
        &provider,
        &AlwaysAllow,
        &opts,
    )
    .await
    .expect("loop ok");

    let seen = format!("{:?}", provider.seen.lock().unwrap()[0]);
    assert!(seen.contains("origin-recall"), "the memory was recalled: {seen}");
    assert!(
        !seen.contains(KEY),
        "raw key reached the model via recall: {seen}"
    );
    assert!(seen.contains(MARKER), "{seen}");
}

/// Finding 3: turn-end gate output (child-process stdout) is redacted before it
/// rides to the model as volatile context or as the repro-gate continuation.
#[tokio::test]
async fn gate_output_echoing_a_key_is_redacted_for_the_model() {
    setup();
    let dir = tempfile::tempdir().unwrap();
    let edited = dir.path().join("lib.py");
    let provider = Recording::new(vec![
        tool_use(
            "Write",
            &serde_json::json!({"file_path": edited.to_str().unwrap(), "content": "x = 1\n"}),
        ),
        Block::text("done"),
        Block::text("done"),
        Block::text("done"),
        Block::text("done"),
    ]);
    let opts = LoopOptions {
        streaming_disabled: true,
        post_edit: Some(Arc::new(origin_postedit::PostEditConfig {
            auto_test: true,
            test_command: Some(format!("echo {KEY}; exit 1")),
            ..origin_postedit::PostEditConfig::default()
        })),
        ..LoopOptions::default()
    };
    let mut session = Session::new("test", "claude-opus-4-7");
    let _ = run_loop(&mut session, "write it", &provider, &AlwaysAllow, &opts).await;

    let seen = provider.seen.lock().unwrap().clone();
    let texts: Vec<String> = seen
        .iter()
        .flatten()
        .flat_map(|m| &m.blocks)
        .filter_map(|b| match b {
            Block::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    let all = texts.join("\n---\n");
    assert!(
        !all.contains(KEY),
        "raw key reached the model via gate output: {all}"
    );
    assert!(all.contains(MARKER), "gate output never reached the model: {all}");
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("[repro-gate]") && t.contains(MARKER)),
        "repro-gate continuation carries the marker: {all}"
    );
}
