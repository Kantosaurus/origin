// SPDX-License-Identifier: Apache-2.0
//! P6.9 — end-to-end test: daemon wires the memory subsystem and a
//! `MemoryDecision::Edit` round-trip persists into `MemoryStore`.
//!
//! Uses wiremock to back the Anthropic provider so no live API key is needed.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::match_wildcard_for_single_variants,
    clippy::too_many_lines
)]

use std::sync::Arc;

use origin_cas::{Store as CasStore, StoreConfig};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::memory_wiring::MemoryWiring;
use origin_daemon::protocol::StreamEvent;
use origin_daemon::session::Session;
use origin_mem::{MemIndex, MemoryStore};
use origin_permission::prompt::AlwaysAllow;
use origin_provider::Provider;
use origin_provider_anthropic::Anthropic;
use origin_store::Store as SqlStore;
use origin_tools::dispatch::MemoryHandle;
use parking_lot::RwLock as PlRwLock;
use serde_json::json;
use tempfile::tempdir;
use tokio::sync::mpsc;
use wiremock::matchers::{method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn memory_e2e_proposer_to_store_via_wiremock() {
    // ── 1. Mock Anthropic ───────────────────────────────────────────────────
    let server = MockServer::start().await;
    let canned = json!({
        "id": "msg_e2e_01",
        "type": "message",
        "role": "assistant",
        "model": "claude-opus-4-7",
        "content": [{ "type": "text", "text": "noted, will keep replies terse." }],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 12,
            "output_tokens": 6,
            "cache_read_input_tokens": 0,
            "cache_creation_input_tokens": 0
        }
    });
    Mock::given(method("POST"))
        .and(wm_path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(canned))
        .mount(&server)
        .await;

    // ── 2. Build the daemon's memory subsystem (no ONNX — graceful degrade) ─
    let tmp = tempdir().expect("tempdir");
    let cas_root = tmp.path().join("cas");
    std::fs::create_dir_all(&cas_root).expect("mkcas");
    let db_path = tmp.path().join("origin.db");

    let cas = Arc::new(
        CasStore::open(StoreConfig {
            root: cas_root,
            hot_capacity: 32,
            warm_pack_target_bytes: 1024 * 1024,
            cold_zstd_level: 3,
        })
        .expect("cas open"),
    );
    let sql = Arc::new(SqlStore::open(&db_path).expect("sql open"));
    let store = Arc::new(MemoryStore::new(sql, Arc::clone(&cas)));
    let index = Arc::new(PlRwLock::new(MemIndex::new()));
    // Embedder intentionally None — exercises the daemon's graceful-degrade
    // path (Injector and Consolidator are then None too).
    let memory = MemoryWiring::new(Arc::clone(&store), None, Arc::clone(&index));
    assert!(memory.injector.is_none(), "no embedder → no injector");

    // ── 3. Run one turn through the agent loop against the wiremock provider ─
    // We bypass streaming because the wiremock responds with a non-SSE body.
    // The Proposer scans both user and assistant text at turn end.
    let provider: Arc<dyn Provider> =
        Arc::new(Anthropic::with_base_url("test-key", &server.uri()).with_cas(Arc::clone(&cas)));
    let proposer = Arc::clone(&memory.proposer);
    let (event_tx, mut event_rx) = mpsc::channel::<StreamEvent>(16);

    let mut session = Session::new("anthropic", "claude-opus-4-7");
    let opts = LoopOptions {
        max_turns: 5,
        cas: Some(Arc::clone(&cas)),
        code_graph: None,
        mem_router: None,
        relay_tx: None,
        streaming_disabled: true,
        proposer: Some(Arc::clone(&proposer)),
        event_tx: Some(event_tx.clone()),
        injector: None, // no embedder wired
        sidecar: None,
        session_store: None,
        proposal_registry: None,
        skills: None,
        skill_catalog: None,
        workflows: None,
        memory_handle: None,
        coordinator: None,
        plan: None,
        goal: Arc::new(tokio::sync::Mutex::new(None)),
        policy: None,
        conseca: None,
        effort: None,
        attachments: Vec::new(),
        system_suffix: None,
        read_only: false,
        router: None,
    };
    let _summary = run_loop(
        &mut session,
        "remember: i prefer terse replies",
        provider.as_ref(),
        &AlwaysAllow,
        &opts,
    )
    .await
    .expect("run_loop ok");

    // Drop the producer-side handles so the event channel closes.
    drop(event_tx);
    drop(opts);

    // ── 4. Drain MemoryProposed events ─────────────────────────────────────
    let mut proposals: Vec<(u32, String, Vec<String>)> = Vec::new();
    while let Some(ev) = event_rx.recv().await {
        if let StreamEvent::MemoryProposed {
            proposal_id,
            body,
            suggested_tags,
        } = ev
        {
            proposals.push((proposal_id, body, suggested_tags));
        }
    }
    assert!(
        !proposals.is_empty(),
        "expected at least one MemoryProposed event"
    );
    assert!(
        proposals
            .iter()
            .any(|(_, body, _)| body.to_lowercase().contains("terse")),
        "expected a proposal body mentioning 'terse', got: {proposals:?}"
    );
    let (accept_id, accept_body, accept_tags) = proposals.into_iter().next().expect("non-empty");

    // ── 5. Simulate the daemon's MemoryDecision::Edit handler ──────────────
    // The Edit variant carries (body, tags) inline — the daemon's
    // handle_memory_decision routes Edit through MemoryDispatchHandle::save.
    // We invoke the same handle directly here so the test stays in-process
    // and doesn't need to spin a transport listener.
    let handle = memory.handle();
    let saved_id = MemoryHandle::save(handle.as_ref(), &accept_body, &accept_tags)
        .expect("save via MemoryDispatchHandle");
    assert!(
        ulid::Ulid::from_string(&saved_id).is_ok(),
        "save returned non-ULID: {saved_id}"
    );
    assert_eq!(accept_id, 1, "first proposal_id is 1");

    // ── 6. Assert persistence: MemoryStore now contains the body ───────────
    let all = store.iter_all().expect("iter_all");
    assert_eq!(all.len(), 1, "expected exactly one persisted memory row");
    let row = &all[0];
    // The preview is truncated at 64 UTF-8 bytes; the accepted body is short
    // enough to fit in full.
    assert!(
        accept_body.starts_with(&row.body_preview) || row.body_preview.starts_with(&accept_body),
        "preview mismatch: preview={:?} body={:?}",
        row.body_preview,
        accept_body
    );

    // ── 7. Re-issue mem_search and confirm we get the saved row back ───────
    let hits = MemoryHandle::search(handle.as_ref(), "terse", 5, false).expect("search");
    assert!(
        hits.iter().any(|h| h.id == saved_id),
        "search did not return the saved id; hits = {hits:?}"
    );
}
