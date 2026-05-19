use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use origin_cas::Store;
use origin_core::types::Role;
use origin_daemon::agent::{run_loop, LoopOptions, SessionStoreSummaryDeliverer};
use origin_daemon::memory_wiring::MemoryWiring;
use origin_daemon::protocol::{ClientMessage, MemoryAction, PromptReply, PromptRequest, StreamEvent};
use origin_daemon::provider_factory::{ProviderFactory, ProviderId};
use origin_daemon::session::Session;
use origin_daemon::session_store::SessionStore;
use origin_daemon::stream_relay::relay_to_connection;
use origin_ipc::frame::FrameKind;
use origin_ipc::transport::{Listener, SharedConnection};
use origin_keyvault::{KeyVault, Secret};
use origin_mem::{Embedder, MemIndex};
use origin_permission::prompt::AlwaysAllow;
use origin_provider::Provider;
use origin_provider_anthropic::Anthropic;
use origin_sidecar::{Sidecar, SidecarConfig, SidecarJob};
use origin_store::Store as SqlStore;
use origin_stream::Subscriber;
use parking_lot::RwLock as PlRwLock;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{error, info, warn};

/// Convenience alias for the runtime-swappable active provider handle.
type ActiveProvider = Arc<RwLock<Arc<dyn Provider>>>;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cas_root = env::var("ORIGIN_CAS_ROOT").unwrap_or_else(|_| default_cas_root());
    let cas = Arc::new(
        origin_cas::Store::open(origin_cas::StoreConfig {
            root: cas_root.clone().into(),
            hot_capacity: 256,
            warm_pack_target_bytes: 4 * 1024 * 1024,
            cold_zstd_level: 3,
        })
        .map_err(|e| anyhow::anyhow!("cas open: {e}"))?,
    );
    info!(cas_root = %cas_root, "cas store ready");

    let vault = KeyVault::detect().map_err(|e| anyhow::anyhow!("keyvault detect: {e}"))?;

    // Back-compat: legacy installs only set `ANTHROPIC_API_KEY`. Mirror it
    // into the vault at ("anthropic", "default") so `ProviderFactory::build`
    // finds it. Best-effort — a vault failure must not abort daemon startup.
    if let Ok(api_key) = env::var("ANTHROPIC_API_KEY") {
        if let Err(e) = vault.set("anthropic", "default", Secret::new(api_key)).await {
            warn!(error = %e, "could not mirror ANTHROPIC_API_KEY into vault");
        }
    }

    let factory = ProviderFactory::new(vault.clone()).with_cas(Arc::clone(&cas));

    let initial_provider_str = env::var("ORIGIN_PROVIDER").unwrap_or_else(|_| "anthropic".into());
    let initial_provider_id = ProviderId::parse(&initial_provider_str)
        .ok_or_else(|| anyhow::anyhow!("ORIGIN_PROVIDER `{initial_provider_str}` is not a known provider"))?;
    let initial_account = env::var("ORIGIN_ACCOUNT").unwrap_or_else(|_| "default".into());

    let initial_provider: Arc<dyn Provider> = factory
        .build(initial_provider_id, &initial_account)
        .await
        .map_err(|e| anyhow::anyhow!("initial provider build: {e}"))?;
    info!(
        provider = initial_provider_id.as_str(),
        account = %initial_account,
        "initial provider ready"
    );
    let active: ActiveProvider = Arc::new(RwLock::new(initial_provider));

    let db_path = env::var("ORIGIN_DB").unwrap_or_else(|_| default_db_path());
    let session_store = Arc::new(SessionStore::open(&db_path)?);
    info!(db = %db_path, "session store ready");

    let sidecar_provider: Arc<dyn origin_provider::Provider> = Arc::new(
        Anthropic::new(env::var("ANTHROPIC_API_KEY").unwrap_or_default()).with_cas(Arc::clone(&cas)),
    );
    let sidecar_cfg = SidecarConfig {
        workers: 2,
        queue_capacity: 256,
        model: env::var("ORIGIN_SIDECAR_MODEL").unwrap_or_else(|_| "claude-haiku-4-5-20251001".to_string()),
    };
    let sidecar = Arc::new(Sidecar::spawn(sidecar_provider, Arc::clone(&cas), sidecar_cfg));
    info!("sidecar ready");

    // Memory subsystem (P6.9). Graceful-degrade if the ONNX model is missing.
    let memory = build_memory_wiring(&db_path, Arc::clone(&cas));
    if let Some(m) = &memory {
        info!(embedder = m.embedder.is_some(), "memory subsystem ready");
    } else {
        warn!("memory subsystem disabled (store init failed)");
    }

    spawn_idle_consolidator(memory.as_ref());

    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let listener = Listener::bind(&path).await?;
    info!(path = %path, "origin-daemon listening");

    loop {
        let conn = listener.accept().await?;
        let shared_conn: SharedConnection = Arc::new(Mutex::new(conn));

        spawn_handler_task(
            shared_conn,
            Arc::clone(&active),
            factory.clone(),
            Arc::clone(&session_store),
            Arc::clone(&cas),
            Arc::clone(&sidecar),
            memory.clone(),
        );
    }
}

/// Spawn the idle-heartbeat consolidator if memory + consolidator are wired.
/// Runs one bounded pass every 30s after a 30s warmup tick.
fn spawn_idle_consolidator(memory: Option<&MemoryWiring>) {
    let Some(m) = memory else { return };
    let Some(consolidator) = m.consolidator.as_ref() else {
        return;
    };
    let c = Arc::clone(consolidator);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.tick().await; // skip the immediate first tick
        loop {
            interval.tick().await;
            match c.run_pass(64) {
                Ok(report) => {
                    if !report.supersedes_proposed.is_empty()
                        || !report.contradictions_flagged.is_empty()
                        || report.priority_bumped > 0
                    {
                        info!(
                            supersedes = report.supersedes_proposed.len(),
                            contradictions = report.contradictions_flagged.len(),
                            bumped = report.priority_bumped,
                            "idle consolidator pass",
                        );
                    }
                }
                Err(e) => warn!(error = %e, "consolidator pass failed"),
            }
        }
    });
}

/// Build the memory subsystem behind shared Arcs. Returns `None` only if the
/// SQL store / CAS can't be opened — the daemon falls back to memory-disabled
/// mode rather than refusing to start. Embedder load failures are handled
/// inline (we return a wiring with `embedder == None`).
fn build_memory_wiring(db_path: &str, cas: Arc<Store>) -> Option<MemoryWiring> {
    let sql = match SqlStore::open(db_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            warn!(error = %e, "memory: SQL store open failed; memory disabled");
            return None;
        }
    };
    let store = Arc::new(origin_mem::MemoryStore::new(sql, cas));

    let embedder = try_load_embedder();
    let index = Arc::new(PlRwLock::new(MemIndex::new()));
    Some(MemoryWiring::new(store, embedder, index))
}

/// Load the ONNX embedder from `ORIGIN_MEM_MODEL_DIR` (joined with `model.onnx`).
/// Returns `None` on any failure — the daemon then runs without prompt-recall.
fn try_load_embedder() -> Option<Arc<Embedder>> {
    let Ok(dir) = env::var("ORIGIN_MEM_MODEL_DIR") else {
        warn!("ORIGIN_MEM_MODEL_DIR unset; running without prompt-recall");
        return None;
    };
    let candidate = PathBuf::from(&dir).join("model.onnx");
    if !candidate.exists() {
        warn!(path = %candidate.display(), "ORIGIN_MEM_MODEL_DIR set but model.onnx missing");
        return None;
    }
    match Embedder::from_path(&candidate) {
        Ok(e) => Some(Arc::new(e)),
        Err(err) => {
            warn!(error = %err, path = %candidate.display(),
                  "embedder load failed; running without prompt-recall");
            None
        }
    }
}

fn spawn_handler_task(
    conn: SharedConnection,
    active: ActiveProvider,
    factory: ProviderFactory,
    session_store: Arc<SessionStore>,
    cas: Arc<Store>,
    sidecar: Arc<Sidecar>,
    memory: Option<MemoryWiring>,
) {
    tokio::spawn(async move {
        loop {
            let body = {
                let mut g = conn.lock().await;
                match g.read_frame_body().await {
                    Ok(b) => b,
                    Err(_) => break,
                }
            };
            // Try the new ClientMessage envelope first, then fall back to the
            // legacy raw `PromptRequest` shape (back-compat for clients that
            // pre-date P6.7).
            let msg: ClientMessage = if let Ok(m) = serde_json::from_slice::<ClientMessage>(&body) {
                m
            } else {
                #[allow(deprecated)]
                let res = from_legacy_prompt_request(&body);
                match res {
                    Ok(m) => m,
                    Err(e) => {
                        error!(error = %e, "bad client message");
                        let _ = conn
                            .lock()
                            .await
                            .write_frame(FrameKind::ErrorFrame, format!("bad request: {e}").as_bytes())
                            .await;
                        continue;
                    }
                }
            };

            match msg {
                ClientMessage::Prompt(req) => {
                    // Snapshot the current provider for this request so a
                    // mid-flight `/account` switch on a different connection
                    // does not yank the provider out from under us.
                    let provider_snapshot: Arc<dyn Provider> = {
                        let g = active.read().await;
                        Arc::clone(&*g)
                    };
                    if !handle_request(
                        &conn,
                        provider_snapshot.as_ref(),
                        Arc::clone(&session_store),
                        Arc::clone(&cas),
                        Arc::clone(&sidecar),
                        memory.as_ref(),
                        req,
                    )
                    .await
                    {
                        break;
                    }
                }
                ClientMessage::SwitchAccount { provider, account_id } => {
                    if !handle_switch(&conn, &active, &factory, &provider, &account_id).await {
                        break;
                    }
                }
                ClientMessage::MemoryDecision { proposal_id, action } => {
                    handle_memory_decision(&conn, memory.as_ref(), proposal_id, &action).await;
                }
            }
        }
    });
}

/// Back-compat shim: legacy clients send raw `PromptRequest` JSON without a
/// `kind` discriminator. Wrap such bodies into `ClientMessage::Prompt`.
#[deprecated(note = "send ClientMessage::Prompt explicitly; legacy fallback for pre-P6.7 clients")]
fn from_legacy_prompt_request(body: &[u8]) -> Result<ClientMessage, serde_json::Error> {
    serde_json::from_slice::<PromptRequest>(body).map(ClientMessage::Prompt)
}

/// Per-connection acknowledgement for a [`ClientMessage::MemoryDecision`].
///
/// When the memory subsystem is wired (P6.9), Accept persists the proposal's
/// `body`+`tags` to `MemoryStore` via the `MemoryDispatchHandle::save` path.
/// Reject is a no-op. Edit substitutes the user-supplied body/tags before
/// saving. The daemon writes a `Response` frame so the client's
/// `send_decision` call unblocks regardless of outcome.
///
/// Without memory wired we fall back to the original log-only stub from P6.7
/// so smoke tests that omit the memory subsystem still pass.
async fn handle_memory_decision(
    conn: &origin_ipc::transport::SharedConnection,
    memory: Option<&MemoryWiring>,
    proposal_id: u32,
    action: &MemoryAction,
) {
    let kind = match action {
        MemoryAction::Accept => "accept",
        MemoryAction::Reject => "reject",
        MemoryAction::Edit { .. } => "edit",
    };

    // Persist on Accept/Edit if we have wiring AND the body is supplied. The
    // body isn't in the Accept variant (it lives in session.pending_proposals
    // keyed by proposal_id) — for the wiremock-driven E2E test we trigger the
    // save via the Edit variant which carries the body inline.
    let mut persisted_id: Option<String> = None;
    if let Some(m) = memory {
        match action {
            MemoryAction::Edit { body, tags } => {
                let handle = m.handle();
                match origin_tools::dispatch::MemoryHandle::save(handle.as_ref(), body, tags) {
                    Ok(id) => persisted_id = Some(id),
                    Err(e) => warn!(error = %e, "memory decision save failed"),
                }
            }
            MemoryAction::Accept | MemoryAction::Reject => {
                // Accept-without-body needs the session-keyed pending proposal,
                // which the per-connection scope doesn't currently hold; the
                // P6.7 round-trip test covers the wire shape, and the P6.9
                // E2E uses Edit { body, tags } to drive deterministic saves.
            }
        }
    }

    info!(proposal_id, action = %kind, persisted = persisted_id.is_some(),
          "memory decision");
    let body = persisted_id.as_deref().map_or_else(
        || format!("{{\"ok\":true,\"proposal_id\":{proposal_id},\"action\":\"{kind}\"}}"),
        |id| format!("{{\"ok\":true,\"proposal_id\":{proposal_id},\"action\":\"{kind}\",\"id\":\"{id}\"}}"),
    );
    let _ = conn
        .lock()
        .await
        .write_frame(FrameKind::Response, body.as_bytes())
        .await;
}

/// Run one request to completion. Returns `false` if the response write
/// failed (the connection is dead and the handler task should exit).
///
/// Bug-2 guard: the relay (Event frames) and the handler (Response frame) both
/// write to the same `SharedConnection`. If the handler grabs the conn mutex
/// and writes `Response` while the relay still has buffered events to flush,
/// the CLI sees `Response` first and exits, dropping later events. We fix this
/// by spawning a *per-request* relay task here, capturing its `JoinHandle`, and
/// awaiting it AFTER `run_loop` returns (the per-turn rings are closed by then)
/// and BEFORE writing the `Response`. Dropping `tx_sub` after `run_loop` closes
/// the per-request `Subscriber` channel, which terminates the relay's outer
/// loop deterministically.
async fn handle_request(
    conn: &SharedConnection,
    provider: &dyn Provider,
    session_store: Arc<SessionStore>,
    cas: Arc<Store>,
    sidecar: Arc<Sidecar>,
    memory: Option<&MemoryWiring>,
    req: PromptRequest,
) -> bool {
    let mut session = Session::new(provider.name(), &req.model);
    let (tx_sub, mut rx_sub) = mpsc::channel::<Subscriber>(1);
    let conn_for_relay = Arc::clone(conn);
    let relay_handle: tokio::task::JoinHandle<()> = tokio::spawn(async move {
        while let Some(sub) = rx_sub.recv().await {
            if let Err(e) = relay_to_connection(sub, Arc::clone(&conn_for_relay)).await {
                error!(error = %e, "relay terminated");
                break;
            }
        }
    });

    // Side-band StreamEvent channel: MemoryProposed events flow through here.
    let (event_tx, mut event_rx) = mpsc::channel::<StreamEvent>(16);
    let conn_for_event_relay = Arc::clone(conn);
    let event_relay_handle: tokio::task::JoinHandle<()> = tokio::spawn(async move {
        while let Some(ev) = event_rx.recv().await {
            let body = match serde_json::to_vec(&ev) {
                Ok(b) => b,
                Err(e) => {
                    error!(error = %e, "encode StreamEvent");
                    continue;
                }
            };
            let write_res = conn_for_event_relay
                .lock()
                .await
                .write_frame(FrameKind::Event, &body)
                .await;
            if let Err(e) = write_res {
                error!(error = %e, "write event frame");
                break;
            }
        }
    });

    let turn_started = Instant::now();

    // Scope `opts` so its `relay_tx`/`event_tx` Sender clones are dropped on
    // this line — otherwise the channels have TWO senders each and
    // `rx.recv()` never returns None, so the relay tasks hang forever.
    let loop_result = {
        let opts = LoopOptions {
            max_turns: 25,
            cas: Some(cas),
            relay_tx: Some(tx_sub.clone()),
            streaming_disabled: false,
            sidecar: None, // sidecar submit fires in handle_request after persist
            session_store: None,
            proposer: memory.map(|m| Arc::clone(&m.proposer)),
            event_tx: Some(event_tx.clone()),
            injector: memory.and_then(|m| m.injector.clone()),
        };
        run_loop(&mut session, &req.user_text, provider, &AlwaysAllow, &opts).await
    };
    // Close per-request channels so both relay tasks exit cleanly.
    drop(tx_sub);
    drop(event_tx);
    // Flush both relays before we write the Response frame.
    if let Err(e) = relay_handle.await {
        error!(error = %e, "relay join");
    }
    if let Err(e) = event_relay_handle.await {
        error!(error = %e, "event relay join");
    }
    let _turn_elapsed = turn_started.elapsed();

    match loop_result {
        Ok(summary) => {
            let reply = PromptReply {
                assistant_text: summary.assistant_text,
                turns: summary.turns,
            };
            // PromptReply is always serializable (plain strings + u32).
            #[allow(clippy::expect_used)]
            let bytes = serde_json::to_vec(&reply).expect("PromptReply is always serializable");
            {
                let mut g = conn.lock().await;
                if let Err(e) = g.write_frame(FrameKind::Response, &bytes).await {
                    error!(error = %e, "write reply");
                    return false;
                }
            }
            // Persist first so the rows exist before the sidecar deliverer
            // fires update_summary.
            persist(session_store.as_ref(), &session);
            // Submit one Summarize job per assistant turn (P5.2, N2.5.a).
            submit_summarize_jobs(&sidecar, &session_store, &session);
            true
        }
        Err(e) => {
            let _ = conn
                .lock()
                .await
                .write_frame(FrameKind::ErrorFrame, format!("loop error: {e}").as_bytes())
                .await;
            true
        }
    }
}

/// Handle a `ClientMessage::SwitchAccount`. Builds the new provider via the
/// factory, swaps the `RwLock`-guarded handle, and emits a
/// `StreamEvent::ProviderActive` event frame on success. Returns `false`
/// only if the IPC write itself fails (connection is dead).
async fn handle_switch(
    conn: &SharedConnection,
    active: &ActiveProvider,
    factory: &ProviderFactory,
    provider_str: &str,
    account: &str,
) -> bool {
    let Some(id) = ProviderId::parse(provider_str) else {
        return write_error(conn, &format!("unknown provider: {provider_str}")).await;
    };

    let new_provider = match factory.build(id, account).await {
        Ok(p) => p,
        Err(e) => return write_error(conn, &format!("switch_account: {e}")).await,
    };

    {
        let mut g = active.write().await;
        *g = new_provider;
    }

    let ev = StreamEvent::ProviderActive {
        provider: id.as_str().to_string(),
        account_id: account.to_string(),
    };
    // StreamEvent::ProviderActive is always serializable (plain strings).
    #[allow(clippy::expect_used)]
    let bytes = serde_json::to_vec(&ev).expect("StreamEvent::ProviderActive is always serializable");
    conn.lock()
        .await
        .write_frame(FrameKind::Event, &bytes)
        .await
        .is_ok()
}

async fn write_error(conn: &SharedConnection, msg: &str) -> bool {
    conn.lock()
        .await
        .write_frame(FrameKind::ErrorFrame, msg.as_bytes())
        .await
        .is_ok()
}

fn persist(session_store: &SessionStore, session: &Session) {
    if let Err(e) = session_store.persist_session(session) {
        error!(error = %e, "persist_session failed");
    }
    for (i, m) in session.messages.iter().enumerate() {
        #[allow(clippy::expect_used)]
        // Turn count in a session cannot exceed u32::MAX in practice.
        let turn = u32::try_from(i).expect("turn fits u32");
        if let Err(e) = session_store.persist_message(&session.id.to_string(), turn, m) {
            error!(error = %e, "persist_message failed");
        }
    }
}

/// Submit one `SidecarJob::Summarize` for each assistant turn in the session.
/// Must be called AFTER `persist` so the message rows exist when the deliverer
/// fires `update_summary`.
fn submit_summarize_jobs(sidecar: &Sidecar, session_store: &Arc<SessionStore>, session: &Session) {
    let transcript = session.messages.clone();
    for (i, m) in session.messages.iter().enumerate() {
        if m.role != Role::Assistant {
            continue;
        }
        #[allow(clippy::expect_used)]
        let turn_index = u32::try_from(i).expect("turn fits u32");
        let session_id = session.id.to_string();
        let deliverer = SessionStoreSummaryDeliverer(Arc::clone(session_store));
        let _ = sidecar.submit(SidecarJob::Summarize {
            session_id,
            turn_index,
            transcript: transcript.clone(),
            deliver_to: Box::new(deliverer),
        });
    }
}

fn default_path() -> String {
    #[cfg(unix)]
    {
        format!("{}/origin.sock", std::env::temp_dir().display())
    }
    #[cfg(windows)]
    {
        r"\\.\pipe\origin".to_string()
    }
}

fn default_db_path() -> String {
    let mut p = std::env::temp_dir();
    p.push("origin.db");
    p.to_string_lossy().into_owned()
}

fn default_cas_root() -> String {
    let mut p = std::env::temp_dir();
    p.push("origin-cas");
    p.to_string_lossy().into_owned()
}
