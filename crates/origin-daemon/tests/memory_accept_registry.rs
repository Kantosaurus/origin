//! `MemoryDecision::Accept` resolves through the daemon-wide
//! [`ProposalRegistry`] (P6.7 follow-up — closes the "Accept-without-body"
//! gap that previously forced the wire shape to use `Edit { body, tags }`).
//!
//! Drives the agent loop with a wiremock-backed Anthropic provider so the
//! Proposer records a candidate, then verifies `ProposalRegistry::take` +
//! `MemoryHandle::save` produce the same on-disk row that `Edit` would have
//! produced.

#![allow(clippy::panic, clippy::expect_used, clippy::too_many_lines)]

use std::sync::Arc;

use origin_cas::{Store as CasStore, StoreConfig};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::memory_wiring::MemoryWiring;
use origin_daemon::proposal_registry::ProposalRegistry;
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
async fn accept_persists_via_proposal_registry() {
    // Mock Anthropic — replies with a plain non-streaming message body.
    let server = MockServer::start().await;
    let canned = json!({
        "id": "msg_accept_01",
        "type": "message",
        "role": "assistant",
        "model": "claude-opus-4-7",
        "content": [{"type":"text","text":"noted; will keep replies terse."}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 3, "output_tokens": 5}
    });
    Mock::given(method("POST"))
        .and(wm_path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(canned))
        .mount(&server)
        .await;

    // Real stores backed by tempdirs.
    let tmp = tempdir().expect("tempdir");
    let cas_root = tmp.path().join("cas");
    let db_path = tmp.path().join("origin.db");
    let cas = Arc::new(
        CasStore::open(StoreConfig {
            root: cas_root,
            hot_capacity: 16,
            warm_pack_target_bytes: 1 << 20,
            cold_zstd_level: 1,
        })
        .expect("cas"),
    );
    let sql = Arc::new(SqlStore::open(&db_path).expect("sql"));
    let mem_store = Arc::new(MemoryStore::new(sql, Arc::clone(&cas)));
    let memory = MemoryWiring::new(
        mem_store,
        None, // no ONNX embedder — naïve search path
        Arc::new(PlRwLock::new(MemIndex::new())),
    );

    // Drive one turn through the agent loop with the registry wired.
    let provider: Arc<dyn Provider> =
        Arc::new(Anthropic::with_base_url("test-key", &server.uri()).with_cas(Arc::clone(&cas)));
    let proposer = Arc::clone(&memory.proposer);
    let registry = Arc::new(ProposalRegistry::new());
    let (event_tx, mut event_rx) = mpsc::channel::<StreamEvent>(16);

    let mut session = Session::new("anthropic", "claude-opus-4-7");
    let opts = LoopOptions {
        max_turns: 5,
        cas: Some(Arc::clone(&cas)),
        relay_tx: None,
        streaming_disabled: true,
        proposer: Some(Arc::clone(&proposer)),
        event_tx: Some(event_tx.clone()),
        injector: None,
        sidecar: None,
        session_store: None,
        proposal_registry: Some(Arc::clone(&registry)),
        skills: None,
        skill_catalog: None,
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
    drop(opts);
    drop(event_tx);

    // Find the emitted proposal id.
    let mut proposal_id: Option<u32> = None;
    while let Some(ev) = event_rx.recv().await {
        if let StreamEvent::MemoryProposed { proposal_id: pid, .. } = ev {
            proposal_id = Some(pid);
            break;
        }
    }
    let pid = proposal_id.expect("Proposer emitted at least one candidate");

    // Simulate `MemoryDecision::Accept` — the daemon-wide registry resolves
    // the body/tags and persists through MemoryHandle::save.
    let pending = registry.take(pid).expect("registry holds the proposal");
    let handle = memory.handle();
    let id = MemoryHandle::save(handle.as_ref(), &pending.body, &pending.tags).expect("save ok");
    assert!(!id.is_empty());

    // Verify it landed by running a search and finding the row.
    let hits = MemoryHandle::search(handle.as_ref(), "terse", 5, false).expect("search ok");
    assert!(
        hits.iter().any(|h| h.preview.to_lowercase().contains("terse")),
        "expected the accepted proposal to be searchable: hits={hits:?}"
    );

    // A second take of the same id is a no-op — already consumed.
    assert!(registry.take(pid).is_none());
}
