use std::env;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use origin_cas::Store;
use origin_core::types::Role;
use origin_daemon::agent::{run_loop, LoopOptions, SessionStoreSummaryDeliverer};
use origin_skills::SkillRegistry;
use origin_swarm::Coordinator;
use origin_daemon::auth::BearerStore;
use origin_daemon::config::bearer_ttl_secs;
use origin_daemon::memory_wiring::MemoryWiring;
use origin_daemon::pairing::{Pairing, RedeemResult};
use origin_daemon::plan_bus::PlanBus;
use origin_daemon::proposal_registry::ProposalRegistry;
use origin_daemon::protocol::{ClientMessage, MemoryAction, PromptReply, PromptRequest, StreamEvent};
use origin_daemon::provider_factory::{ProviderFactory, ProviderId};
use origin_daemon::runtime_launch::{self, ShutdownSignal};
use origin_daemon::session::Session;
use origin_daemon::session_store::SessionStore;
use origin_daemon::shutdown::{CooperativeShutdown, ShutdownPhase};
use origin_daemon::skill_catalog::SkillCatalog;
use origin_daemon::stream_relay::relay_to_connection;
use origin_ipc::frame::FrameKind;
use origin_ipc::transport::{Listener, SharedConnection};
use origin_keyvault::{KeyVault, Secret};
use origin_mem::{Embedder, MemIndex};
use origin_metrics::Metrics;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::catalog::Catalog;
use origin_provider::Provider;
use origin_provider_anthropic::Anthropic;
use origin_runtime::{spawn_in, TaskClass};
use origin_sidecar::{Sidecar, SidecarConfig, SidecarJob};
use origin_store::Store as SqlStore;
use origin_stream::Subscriber;
use parking_lot::RwLock as PlRwLock;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{error, info, warn};

/// Convenience alias for the runtime-swappable active provider handle.
type ActiveProvider = Arc<RwLock<Arc<dyn Provider>>>;

/// Snapshot of subsystem handles the cooperative shutdown driver binds to.
///
/// `main` constructs one [`Arc<Mutex<DaemonState>>`] up front and hands it to
/// both `daemon_setup` (which populates the fields as each subsystem comes
/// up) and the control-core signal-handler task (which reads them when a
/// shutdown signal fires). Fields are `Option` because the daemon may be
/// shutting down mid-boot, before every subsystem is wired.
#[derive(Default)]
struct DaemonState {
    sidecar: Option<Arc<Sidecar>>,
    cas: Option<Arc<Store>>,
    session_store: Option<Arc<SessionStore>>,
    /// Set to `true` by the `StopAcceptingIpc` phase. The accept loop polls
    /// this between accepts so new connections stop being served.
    accept_disabled: Option<Arc<AtomicBool>>,
}

/// Hand-rolled entrypoint — replaces `#[tokio::main]` with the P12.8
/// two-runtime split. The control core runs on a dedicated `origin-ctrl`
/// OS thread; the worker pool gets `physical_cores - 1` workers. The
/// existing async pipeline (`daemon_setup`) runs on the worker pool via
/// `spawn_on_worker` + `Handle::block_on`. SIGINT/SIGTERM trigger the
/// shutdown signal; the full phased shutdown lands in P12.11.
fn main() -> Result<()> {
    // Install the parquet-backed tracing layer. The guard holds the drain
    // thread alive for the lifetime of `main`; on shutdown it flushes any
    // buffered spans before exiting. We keep this on the OS main thread so
    // its Drop runs after both runtimes have torn down.
    let trace_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("origin")
        .join("trace");
    let _trace_guard =
        origin_trace::init(&trace_dir).map_err(|e| anyhow::anyhow!("origin-trace init: {e}"))?;

    let signal = ShutdownSignal::new();
    let state: Arc<std::sync::Mutex<DaemonState>> = Arc::new(std::sync::Mutex::new(DaemonState::default()));

    // Spawn the two-runtime launcher on its own OS thread. It blocks until
    // `signal.trigger()` is called, then tears down both runtimes.
    let signal_for_launcher = signal.clone();
    let launcher_join = std::thread::Builder::new()
        .name("origin-launcher".to_string())
        .spawn(move || runtime_launch::start(signal_for_launcher))
        .map_err(|e| anyhow::anyhow!("launcher thread spawn: {e}"))?;

    // Wait for the worker handle to be populated. `start()` sets it before
    // spawning the control thread, but we run concurrently — poll briefly.
    let worker_handle = wait_for_worker_handle(&signal)?;

    // Hand the existing async setup to the worker pool. We use the worker
    // handle's `block_on` from a blocking task so the daemon's main accept
    // loop runs on the worker runtime, not the control core.
    let signal_for_setup = signal.clone();
    let state_for_setup = Arc::clone(&state);
    worker_handle.spawn_blocking(move || {
        let h = signal_for_setup
            .worker_handle()
            .raw()
            .expect("worker handle populated");
        if let Err(e) = h.block_on(daemon_setup(state_for_setup)) {
            error!(error = %e, "daemon_setup terminated with error");
            signal_for_setup.trigger();
        }
    });

    // P12.11: SIGINT/SIGTERM/Ctrl+C wakes a tokio watcher that drives the
    // CooperativeShutdown phases on the control core, then triggers the
    // shutdown signal so the launcher returns. The current
    // `CooperativeShutdown::for_production` is a stub equivalent to
    // `for_test(no_op_channel, 30s_budget)` — full per-phase wiring (IPC
    // listener stop, sidecar queue persist, CAS flush, SQLite checkpoint,
    // etc.) is a P14 polish item.
    //
    // We still use `ctrlc` as the cross-platform signal entry — its handler
    // runs on its own thread, posts to a `mpsc` channel, and a control-core
    // task drives the phased shutdown from there. This avoids tying
    // `tokio::signal::ctrl_c` to the OS main thread (which is not a tokio
    // runtime) while still landing the phase driver on the control core.
    let (sig_tx, mut sig_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let signal_for_handler = signal.clone();
    ctrlc::set_handler(move || {
        // Best-effort; if the receiver is dropped the launcher is already on
        // its way out, so we fall back to a direct trigger.
        if sig_tx.send(()).is_err() {
            signal_for_handler.trigger();
        }
    })
    .map_err(|e| anyhow::anyhow!("ctrlc set_handler: {e}"))?;

    let signal_for_shutdown = signal.clone();
    let state_for_shutdown = Arc::clone(&state);
    signal.control_handle().spawn_on_control(async move {
        if sig_rx.recv().await.is_none() {
            // Channel closed without a signal — nothing to do.
            return;
        }
        let mut driver = build_shutdown_driver(&state_for_shutdown);
        match driver.run().await {
            Ok(report) => info!(?report, "cooperative shutdown complete"),
            Err(e) => warn!(error = %e, "cooperative shutdown driver returned error"),
        }
        signal_for_shutdown.trigger();
    });

    // Block on the launcher; it returns when `signal.trigger()` fires.
    launcher_join
        .join()
        .map_err(|_| anyhow::anyhow!("launcher thread panicked"))?;
    Ok(())
}

/// Poll the worker handle until `start()` populates it. The launcher sets
/// the handle synchronously inside `start()` before the control thread
/// spawns, so this typically resolves in microseconds.
fn wait_for_worker_handle(signal: &ShutdownSignal) -> Result<tokio::runtime::Handle> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(h) = signal.worker_handle().raw() {
            return Ok(h);
        }
        if Instant::now() >= deadline {
            return Err(anyhow::anyhow!("worker handle never came up"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The pre-P12.8 daemon body, lifted verbatim (modulo `_trace_guard` which
/// now lives in `main`). Runs to completion on the worker pool.
//
// Cognitive complexity exceeds the workspace's nursery threshold after the
// P12 + P13 merge — both phases added new IPC verbs (`PairStart`/`PairRedeem`
// from P13; `ResumeRequest` from P12) and supporting wiring to this single
// entrypoint. Breaking it apart is P14 polish (see plan).
/// Assemble a [`CooperativeShutdown`] driver whose per-phase callbacks
/// run against whichever subsystems `daemon_setup` has populated in
/// `state`. Subsystems wired here today:
///
/// - **`StopAcceptingIpc`** flips an `AtomicBool` polled by the accept loop.
/// - **`PersistSidecarQueue`** drains in-flight `SidecarJob`s by dropping the
///   queue's sender and awaiting workers (`Sidecar::shutdown`).
/// - **`FlushCasWriteBuffer`** writes pending warm-pack bytes to disk.
/// - **`CheckpointSqlite`** runs `PRAGMA wal_checkpoint(TRUNCATE)`.
///
/// Phases without an installed subsystem fall back to the
/// `yield_now`-only no-op the driver uses for tests.
fn build_shutdown_driver(state: &Arc<std::sync::Mutex<DaemonState>>) -> CooperativeShutdown {
    let snapshot = {
        let g = state.lock().expect("daemon state mutex");
        DaemonStateSnapshot {
            sidecar: g.sidecar.clone(),
            cas: g.cas.clone(),
            session_store: g.session_store.clone(),
            accept_disabled: g.accept_disabled.clone(),
        }
    };
    let mut driver = CooperativeShutdown::for_production();
    if let Some(flag) = snapshot.accept_disabled.clone() {
        driver = driver.on(ShutdownPhase::StopAcceptingIpc, move || async move {
            flag.store(true, Ordering::Release);
            info!("shutdown: stopped accepting new IPC connections");
        });
    }
    if let Some(sidecar) = snapshot.sidecar.clone() {
        driver = driver.on(ShutdownPhase::PersistSidecarQueue, move || async move {
            sidecar.shutdown().await;
            info!("shutdown: sidecar queue drained");
        });
    }
    if let Some(cas) = snapshot.cas.clone() {
        driver = driver.on(ShutdownPhase::FlushCasWriteBuffer, move || async move {
            if let Err(e) = cas.flush_warm_pending() {
                warn!(error = %e, "shutdown: cas flush_warm_pending failed");
            } else {
                info!("shutdown: cas warm-pending bytes flushed");
            }
        });
    }
    if let Some(store) = snapshot.session_store {
        driver = driver.on(ShutdownPhase::CheckpointSqlite, move || async move {
            if let Err(e) = store.checkpoint() {
                warn!(error = %e, "shutdown: sqlite checkpoint failed");
            } else {
                info!("shutdown: sqlite WAL checkpointed");
            }
        });
    }
    driver
}

/// Plain (non-Mutex) snapshot of [`DaemonState`] used inside the closures
/// captured by [`build_shutdown_driver`]. Keeping a value-typed clone here
/// lets the move closures own only the handles they need.
struct DaemonStateSnapshot {
    sidecar: Option<Arc<Sidecar>>,
    cas: Option<Arc<Store>>,
    session_store: Option<Arc<SessionStore>>,
    accept_disabled: Option<Arc<AtomicBool>>,
}

// `daemon_setup` deliberately wires every subsystem in one place — splitting
// it out is a P14 polish item already noted above. Allow the line count
// linter to ignore this wiring function.
#[allow(clippy::cognitive_complexity, clippy::too_many_lines)]
async fn daemon_setup(state: Arc<std::sync::Mutex<DaemonState>>) -> Result<()> {
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

    // P13.2: pairing + bearer-token state lives in-process. Both handles
    // are cloned (Arc) into each per-connection future so concurrent
    // pair_start / pair_redeem calls share one state machine.
    let pairing = Arc::new(Pairing::new());
    let bearer_store = Arc::new(BearerStore::new());
    // Daemon-wide pending-proposal registry — lets `MemoryDecision::Accept`
    // resolve a proposal recorded by an earlier prompt-turn handler.
    let proposal_registry = Arc::new(ProposalRegistry::new());
    // Daemon-wide plan-op broadcast bus. IPC clients subscribe via
    // `ClientMessage::SubscribePlan`; swarm coordinators publish via
    // `bus.publish(envelope)` when their PlanHandle::apply succeeds.
    let plan_bus = PlanBus::new();

    // Back-compat: legacy installs only set `ANTHROPIC_API_KEY`. Mirror it
    // into the vault at ("anthropic", "default") so `ProviderFactory::build`
    // finds it. Best-effort — a vault failure must not abort daemon startup.
    if let Ok(api_key) = env::var("ANTHROPIC_API_KEY") {
        if let Err(e) = vault.set("anthropic", "default", Secret::new(api_key)).await {
            warn!(error = %e, "could not mirror ANTHROPIC_API_KEY into vault");
        }
    }

    let mut catalog = Catalog::builtin();
    let cfg_path = dirs::home_dir().map(|h| h.join(".origin").join("providers.toml"));
    if let Some(p) = cfg_path {
        match origin_provider::custom::load(&p) {
            Ok(custom) => {
                if let Err(e) = catalog.merge_custom(custom) {
                    tracing::warn!(target: "origin::provider", error = %e, "custom providers merge failed");
                }
            }
            Err(e) => tracing::warn!(target: "origin::provider", error = %e, "failed to load providers.toml"),
        }
    }
    let factory = ProviderFactory::new(vault.clone(), catalog).with_cas(Arc::clone(&cas));

    let initial_provider_str = env::var("ORIGIN_PROVIDER").unwrap_or_else(|_| "anthropic".into());
    let initial_provider_id = ProviderId::parse(&initial_provider_str, factory.catalog())
        .ok_or_else(|| anyhow::anyhow!("ORIGIN_PROVIDER `{initial_provider_str}` is not a known provider"))?;
    let initial_account = env::var("ORIGIN_ACCOUNT").unwrap_or_else(|_| "default".into());

    let initial_provider: Arc<dyn Provider> = factory
        .build(&initial_provider_id, &initial_account)
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

    // Swarm coordinator (P9.6). We open a dedicated SqlStore for the plan op-log
    // so the plan tables are isolated from the session-store connection.
    // `PlanStore::open` is currently infallible (returns `Ok(Self)` always) so
    // we unwrap rather than abort startup on a construction error.
    let coordinator: Arc<Coordinator> = {
        let plan_sql = match origin_store::Store::open(&db_path) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                warn!(error = %e, "swarm: plan SqlStore open failed; Task tool disabled");
                // Fall through with a dummy coordinator constructed from an in-memory store.
                // We use a tempfile path that will be cleaned up on exit.
                let tmp_db = std::env::temp_dir().join("origin-plan-fallback.db");
                Arc::new(
                    origin_store::Store::open(&tmp_db)
                        .map_err(|e2| anyhow::anyhow!("plan fallback store open: {e2}"))?,
                )
            }
        };
        let plan = Arc::new(tokio::sync::Mutex::new(origin_plan::Plan::new()));
        let plan_store = Arc::new(
            origin_plan::PlanStore::open(plan_sql, Arc::clone(&cas))
                .map_err(|e| anyhow::anyhow!("plan store open: {e}"))?,
        );
        let plan_handle = origin_swarm::PlanHandle::new(plan, plan_store);
        Arc::new(Coordinator::new(plan_handle, "origin-daemon"))
    };
    info!("swarm coordinator ready");

    let skill_catalog: Arc<SkillCatalog> = {
        let home = std::env::var_os("ORIGIN_HOME")
            .map(std::path::PathBuf::from)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let path = home.join(".origin").join("skills");
        SkillCatalog::load_or_empty(&path)
    };
    info!(
        skill_count = skill_catalog.len(),
        "skill catalog loaded at startup"
    );

    spawn_idle_consolidator(memory.as_ref());

    // P11.12: optional bounded-cardinality Prometheus `/metrics` endpoint.
    // Bind address comes from `--metrics-bind <addr>` CLI flag, or, when
    // absent, the `ORIGIN_METRICS_BIND` env var.
    //
    // P13.4.2: we now keep a daemon-wide `Arc<Metrics>` handle regardless of
    // whether the HTTP endpoint is bound, so the admin IPC `GetUsage`
    // handler can read the same registry the `/metrics` exporter does.
    let metrics = Arc::new(Metrics::new());
    if let Some(bind) = parse_metrics_bind() {
        spawn_metrics_endpoint((*metrics).clone(), bind);
    }

    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let listener = Listener::bind(&path).await?;
    info!(path = %path, "origin-daemon listening");

    // Populate the shared `DaemonState` so the cooperative shutdown driver
    // can bind real per-phase callbacks. We do this AFTER each subsystem is
    // up so an early shutdown signal doesn't grab half-initialized handles.
    let accept_disabled = Arc::new(AtomicBool::new(false));
    {
        let mut g = state.lock().expect("daemon state mutex");
        g.sidecar = Some(Arc::clone(&sidecar));
        g.cas = Some(Arc::clone(&cas));
        g.session_store = Some(Arc::clone(&session_store));
        g.accept_disabled = Some(Arc::clone(&accept_disabled));
    }

    loop {
        if accept_disabled.load(Ordering::Acquire) {
            info!("origin-daemon: accept loop stopping after StopAcceptingIpc");
            break Ok(());
        }
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
            Arc::clone(&pairing),
            Arc::clone(&bearer_store),
            vault.clone(),
            Arc::clone(&metrics),
            Arc::clone(&proposal_registry),
            plan_bus.clone(),
            Arc::clone(&skill_catalog),
            Arc::clone(&coordinator),
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
    spawn_in(TaskClass::Background, async move {
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

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn spawn_handler_task(
    conn: SharedConnection,
    active: ActiveProvider,
    factory: ProviderFactory,
    session_store: Arc<SessionStore>,
    cas: Arc<Store>,
    sidecar: Arc<Sidecar>,
    memory: Option<MemoryWiring>,
    pairing: Arc<Pairing>,
    bearer_store: Arc<BearerStore>,
    vault: KeyVault,
    metrics: Arc<Metrics>,
    proposal_registry: Arc<ProposalRegistry>,
    plan_bus: PlanBus,
    skill_catalog: Arc<SkillCatalog>,
    coordinator: Arc<Coordinator>,
) {
    spawn_in(TaskClass::Critical, async move {
        // Per-connection skill activation state. Each ActivateSkill mutates
        // this registry; each Prompt reads its `allowed_tools` mask and passes
        // it through LoopOptions.skills so the permission engine narrows
        // accordingly. Wrapped in Arc<Mutex<...>> so we can hand `Arc::clone`s
        // to async handlers without giving up the registry.
        let active_skills: Arc<tokio::sync::Mutex<SkillRegistry>> =
            Arc::new(tokio::sync::Mutex::new(SkillRegistry::new()));

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
                        Arc::clone(&proposal_registry),
                        Arc::clone(&skill_catalog),
                        Arc::clone(&active_skills),
                        Arc::clone(&coordinator),
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
                    handle_memory_decision(
                        &conn,
                        memory.as_ref(),
                        proposal_registry.as_ref(),
                        proposal_id,
                        &action,
                    )
                    .await;
                }
                ClientMessage::PairStart { ttl_secs } => {
                    let session = pairing.start(Duration::from_secs(u64::from(ttl_secs)));
                    let ev = StreamEvent::PairCode {
                        code: session.code,
                        expires_in_secs: ttl_secs,
                    };
                    if write_event(&conn, &ev).await.is_err() {
                        break;
                    }
                }
                ClientMessage::PairRedeem { code, device_id } => {
                    let ev = match pairing.redeem(&code, &device_id) {
                        Ok(RedeemResult::Issued { bearer, device_id }) => {
                            bearer_store.insert(bearer.clone(), device_id.clone());
                            // Best-effort vault mirror so bearers survive a
                            // daemon restart. A failure here is non-fatal —
                            // the in-memory BearerStore still authorizes
                            // until the daemon exits.
                            if let Err(e) = vault
                                .set("origin-remote", &device_id, Secret::new(bearer.clone()))
                                .await
                            {
                                warn!(error = %e, device = %device_id,
                                      "pair: keyvault mirror failed");
                            }
                            StreamEvent::PairIssued {
                                bearer,
                                device_id,
                                ttl_secs: bearer_ttl_secs(),
                            }
                        }
                        Err(e) => StreamEvent::PairError {
                            message: e.to_string(),
                        },
                    };
                    if write_event(&conn, &ev).await.is_err() {
                        break;
                    }
                }
                ClientMessage::ResumeRequest { token } => {
                    handle_resume_request(&conn, Arc::clone(&session_store), token).await;
                }
                ClientMessage::ActivateSkill { name } => {
                    // Look up the skill in the daemon-wide catalog loaded at
                    // startup. The catalog is the single source of truth shared
                    // with the system-prompt injection (see agent.rs::run_loop).
                    let conn_clone = Arc::clone(&conn);
                    if let Some(skill) = skill_catalog.find(&name) {
                        let front = skill.front.clone();
                        let allowed_tools: Vec<String> = {
                            let mut guard = active_skills.lock().await;
                            guard.activate(front);
                            // After activation, the intersection always exists (we just pushed a skill).
                            // Sort for stable wire output so clients see a deterministic order.
                            guard.allowed_tools().map(|set| {
                                let mut v: Vec<String> = set.into_iter().collect();
                                v.sort();
                                v
                            }).unwrap_or_default()
                        };
                        let ev = StreamEvent::SkillActive {
                            name: name.clone(),
                            allowed_tools,
                        };
                        let body = serde_json::to_vec(&ev).unwrap_or_default();
                        let _ = conn_clone
                            .lock()
                            .await
                            .write_frame(FrameKind::Event, &body)
                            .await;
                    } else {
                        let ev = StreamEvent::SkillError {
                            message: format!("no such skill: {name}"),
                        };
                        let body = serde_json::to_vec(&ev).unwrap_or_default();
                        let _ = conn_clone
                            .lock()
                            .await
                            .write_frame(FrameKind::Event, &body)
                            .await;
                    }
                }
                ClientMessage::DeactivateSkill { name } => {
                    active_skills.lock().await.deactivate(&name);
                    let body = serde_json::to_vec(&StreamEvent::AdminOk).unwrap_or_default();
                    let _ = conn
                        .lock()
                        .await
                        .write_frame(FrameKind::Event, &body)
                        .await;
                }
                ClientMessage::ActivateWorkflow { name } => {
                    let conn_clone = Arc::clone(&conn);
                    // Load workflows.toml fresh so user edits land without a
                    // daemon restart.
                    let home = std::env::var_os("ORIGIN_HOME")
                        .map(std::path::PathBuf::from)
                        .or_else(dirs::home_dir)
                        .unwrap_or_else(|| std::path::PathBuf::from("."));
                    let wf_path = home.join(".origin").join("workflows.toml");
                    let file = match origin_daemon::workflows::load_from(&wf_path) {
                        Ok(f) => f,
                        Err(e) => {
                            let ev = StreamEvent::SkillError {
                                message: format!("workflows.toml load: {e}"),
                            };
                            let body = serde_json::to_vec(&ev).unwrap_or_default();
                            let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
                            continue;
                        }
                    };
                    let Some(wf) = file.workflows.iter().find(|w| w.name == name) else {
                        let ev = StreamEvent::SkillError {
                            message: format!("no such workflow: {name}"),
                        };
                        let body = serde_json::to_vec(&ev).unwrap_or_default();
                        let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
                        continue;
                    };
                    let mut activated: Vec<String> = Vec::new();
                    let mut skipped: Vec<String> = Vec::new();
                    for step in &wf.steps {
                        if let Some(skill) = skill_catalog.find(&step.skill) {
                            active_skills.lock().await.activate(skill.front.clone());
                            activated.push(step.skill.clone());
                        } else {
                            // Partial chain still useful — collect the misses
                            // and surface them in the single ack frame so the
                            // CLI doesn't need a multi-frame read loop.
                            skipped.push(step.skill.clone());
                        }
                    }
                    let ev = StreamEvent::WorkflowActive {
                        name: name.clone(),
                        steps: activated,
                        skipped,
                    };
                    let body = serde_json::to_vec(&ev).unwrap_or_default();
                    let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
                }
                ClientMessage::SubscribePlan => {
                    spawn_plan_relay(plan_bus.subscribe(), Arc::clone(&conn));
                }
                admin @ (ClientMessage::ListSessions
                | ClientMessage::RemoveSession { .. }
                | ClientMessage::ResumeSession { .. }
                | ClientMessage::GetUsage
                | ClientMessage::KeyringAdd { .. }
                | ClientMessage::KeyringList { .. }
                | ClientMessage::KeyringRemove { .. }) => {
                    if !handle_admin(&conn, &session_store, &vault, metrics.as_ref(), admin).await {
                        break;
                    }
                }
            }
        }
    });
}

/// Handle a [`ClientMessage::ResumeRequest`] from the supervisor.
///
/// We persist the supervisor's resume token, then load the persisted message
/// log up to `token.last_turn` so the next `Prompt` against this session
/// reads a hydrated transcript. The acknowledgement carries the actual
/// `restored_to_turn` the daemon hydrated (capped by what's on disk), not
/// just the token's claim. Pending-tool-call re-spawn under
/// `TaskClass::Critical` still requires the in-flight per-tool state the
/// supervisor doesn't checkpoint today — that lands when the supervisor
/// extends `ResumeToken::pending_tool_calls`.
async fn handle_resume_request(
    conn: &SharedConnection,
    session_store: Arc<SessionStore>,
    token: origin_resume_token::ResumeToken,
) {
    let session_id = token.session_id.clone();
    if let Err(e) = session_store.save_resume_token(&token) {
        warn!(error = %e, session = %session_id, "resume: could not persist token");
    }
    let messages = match session_store.load_messages(&session_id) {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, session = %session_id, "resume: load_messages failed");
            Vec::new()
        }
    };
    // Cap the reported turn by what is actually on disk so the supervisor
    // does not believe we hydrated beyond the persisted log.
    #[allow(clippy::cast_possible_truncation)]
    let on_disk_high_watermark = u32::try_from(messages.len()).unwrap_or(u32::MAX);
    let restored_to_turn = token.last_turn.min(on_disk_high_watermark.saturating_sub(1));
    info!(
        session = %session_id,
        last_turn = token.last_turn,
        messages = messages.len(),
        restored_to_turn,
        "resume: ack"
    );
    let ack = origin_daemon::protocol::ServerMessage::ResumeAck {
        session_id,
        restored_to_turn,
    };
    // ServerMessage::ResumeAck is always serializable (plain strings + u32).
    #[allow(clippy::expect_used)]
    let bytes = serde_json::to_vec(&ack).expect("ServerMessage::ResumeAck serializable");
    let _ = conn.lock().await.write_frame(FrameKind::Response, &bytes).await;
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
    proposal_registry: &ProposalRegistry,
    proposal_id: u32,
    action: &MemoryAction,
) {
    let kind = match action {
        MemoryAction::Accept => "accept",
        MemoryAction::Reject => "reject",
        MemoryAction::Edit { .. } => "edit",
    };

    // Resolve the `(body, tags)` to persist. `Edit` carries them inline;
    // `Accept` looks them up in the daemon-wide `ProposalRegistry`; `Reject`
    // drops the registry entry without persisting.
    let mut persisted_id: Option<String> = None;
    let to_persist: Option<(String, Vec<String>)> = match action {
        MemoryAction::Edit { body, tags } => Some((body.clone(), tags.clone())),
        MemoryAction::Accept => proposal_registry.take(proposal_id).map(|p| (p.body, p.tags)),
        MemoryAction::Reject => {
            proposal_registry.drop(proposal_id);
            None
        }
    };
    if let (Some((body, tags)), Some(m)) = (to_persist.as_ref(), memory) {
        let handle = m.handle();
        match origin_tools::dispatch::MemoryHandle::save(handle.as_ref(), body, tags) {
            Ok(id) => persisted_id = Some(id),
            Err(e) => warn!(error = %e, "memory decision save failed"),
        }
    }
    // `Edit` overrides any registry entry — drop the stale one so it doesn't
    // get re-accepted later. `Accept` already removed it via `take`.
    if matches!(action, MemoryAction::Edit { .. }) {
        proposal_registry.drop(proposal_id);
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
// `handle_request` threads each subsystem handle a `Prompt` may touch in one
// signature. Bundling them into a struct is a P14 polish item.
#[allow(clippy::too_many_arguments)]
async fn handle_request(
    conn: &SharedConnection,
    provider: &dyn Provider,
    session_store: Arc<SessionStore>,
    cas: Arc<Store>,
    sidecar: Arc<Sidecar>,
    memory: Option<&MemoryWiring>,
    proposal_registry: Arc<ProposalRegistry>,
    skill_catalog: Arc<SkillCatalog>,
    active_skills: Arc<tokio::sync::Mutex<SkillRegistry>>,
    coordinator: Arc<Coordinator>,
    req: PromptRequest,
) -> bool {
    let mut session = Session::new(provider.name(), &req.model);
    let (tx_sub, mut rx_sub) = mpsc::channel::<Subscriber>(1);
    let conn_for_relay = Arc::clone(conn);
    let relay_handle: tokio::task::JoinHandle<()> = spawn_in(TaskClass::Realtime, async move {
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
    let event_relay_handle: tokio::task::JoinHandle<()> = spawn_in(TaskClass::Realtime, async move {
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
            proposal_registry: Some(Arc::clone(&proposal_registry)),
            skills: {
                // Snapshot the current active-skill stack into a fresh
                // SkillRegistry. We deep-clone the stack via re-activation
                // because the agent loop wants a `&SkillRegistry`, and we
                // don't want to hold the per-connection lock across an
                // arbitrarily long turn.
                let guard = active_skills.lock().await;
                if guard.allowed_tools().is_some() {
                    let mut snapshot = SkillRegistry::new();
                    for s in guard.iter_active() {
                        snapshot.activate(s.clone());
                    }
                    Some(Arc::new(snapshot))
                } else {
                    None
                }
            },
            skill_catalog: Some(Arc::clone(&skill_catalog)),
            coordinator: Some(Arc::clone(&coordinator)),
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
    let Some(id) = ProviderId::parse(provider_str, factory.catalog()) else {
        return write_error(conn, &format!("unknown provider: {provider_str}")).await;
    };

    let new_provider = match factory.build(&id, account).await {
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

/// Dispatch the P13.4.2 admin `ClientMessage` variants. Returns `false`
/// only if the IPC write fails (the per-connection handler then exits).
///
/// Variants other than `ListSessions / RemoveSession / ResumeSession /
/// GetUsage / KeyringAdd / KeyringList / KeyringRemove` are unreachable
/// because the caller restricts the input via an `@`-bound pattern.
async fn handle_admin(
    conn: &SharedConnection,
    session_store: &SessionStore,
    vault: &KeyVault,
    metrics: &Metrics,
    msg: ClientMessage,
) -> bool {
    let ev = match msg {
        ClientMessage::ListSessions => {
            let summaries = session_store.list_summaries().unwrap_or_default();
            let wire: Vec<_> = summaries
                .into_iter()
                .map(|s| origin_daemon::protocol::SessionSummaryWire {
                    id: s.id,
                    created_at: s.created_at,
                    title: s.title,
                    model: s.model,
                    message_count: s.message_count,
                })
                .collect();
            StreamEvent::SessionsListed { summaries: wire }
        }
        ClientMessage::RemoveSession { session_id } => match session_store.delete(&session_id) {
            Ok(()) => StreamEvent::AdminOk,
            Err(e) => StreamEvent::AdminError {
                message: e.to_string(),
            },
        },
        ClientMessage::ResumeSession { session_id } => resume_session_event(session_store, &session_id),
        ClientMessage::GetUsage => StreamEvent::UsageReport {
            rows: build_usage_rows(&metrics.snapshot()),
        },
        ClientMessage::KeyringAdd {
            provider,
            account,
            secret,
        } => match vault.set(&provider, &account, Secret::new(secret)).await {
            Ok(()) => StreamEvent::AdminOk,
            Err(e) => StreamEvent::AdminError {
                message: e.to_string(),
            },
        },
        ClientMessage::KeyringList { provider } => match vault.list(&provider).await {
            Ok(accounts) => StreamEvent::KeyringAccounts { provider, accounts },
            Err(e) => StreamEvent::AdminError {
                message: e.to_string(),
            },
        },
        ClientMessage::KeyringRemove { provider, account } => match vault.delete(&provider, &account).await {
            Ok(()) => StreamEvent::AdminOk,
            Err(e) => StreamEvent::AdminError {
                message: e.to_string(),
            },
        },
        // The caller restricts inputs via `admin @ (...)` so other variants
        // never reach this function.
        ClientMessage::Prompt(_)
        | ClientMessage::SwitchAccount { .. }
        | ClientMessage::MemoryDecision { .. }
        | ClientMessage::PairStart { .. }
        | ClientMessage::PairRedeem { .. }
        | ClientMessage::ResumeRequest { .. }
        | ClientMessage::SubscribePlan
        | ClientMessage::ActivateSkill { .. }
        | ClientMessage::DeactivateSkill { .. }
        | ClientMessage::ActivateWorkflow { .. } => return true,
    };
    write_event(conn, &ev).await.is_ok()
}

/// Fold a `Metrics::snapshot()` into one `UsageRow` per (provider, model)
/// tuple. `origin_tokens_in_total{provider,model}` rows fill the
/// `tokens_in` column; `origin_tokens_out_total{provider,model}` rows
/// fill the `tokens_out` column. Other families are ignored.
fn build_usage_rows(snap: &origin_metrics::Snapshot) -> Vec<origin_daemon::protocol::UsageRow> {
    use std::collections::BTreeMap;
    let mut acc: BTreeMap<(String, String), (u64, u64)> = BTreeMap::new();
    for r in snap.iter() {
        let is_in = r.name == "origin_tokens_in_total";
        let is_out = r.name == "origin_tokens_out_total";
        if !(is_in || is_out) {
            continue;
        }
        let mut provider = String::new();
        let mut model = String::new();
        for (k, v) in &r.labels {
            match k.as_str() {
                "provider" => provider.clone_from(v),
                "model" => model.clone_from(v),
                _ => {}
            }
        }
        // Saturating cast — counter values are non-negative.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let val = r.value as u64;
        let entry = acc.entry((provider, model)).or_insert((0, 0));
        if is_in {
            entry.0 = entry.0.saturating_add(val);
        } else {
            entry.1 = entry.1.saturating_add(val);
        }
    }
    acc.into_iter()
        .map(
            |((provider, model), (tokens_in, tokens_out))| origin_daemon::protocol::UsageRow {
                provider,
                model,
                tokens_in,
                tokens_out,
            },
        )
        .collect()
}

/// Forward every plan op the bus broadcasts to `conn` as a
/// [`StreamEvent::PlanOp`] frame. The task exits when the bus channel is
/// closed (all senders dropped — never under normal operation) or when the
/// connection's write fails (peer disconnected). Lagged subscribers log a
/// warning and resume on the next op; clients should re-snapshot.
fn spawn_plan_relay(
    mut rx: tokio::sync::broadcast::Receiver<origin_plan::OpEnvelope>,
    conn: SharedConnection,
) {
    spawn_in(TaskClass::Realtime, async move {
        loop {
            match rx.recv().await {
                Ok(envelope) => {
                    let ev = StreamEvent::PlanOp { envelope };
                    if write_event(&conn, &ev).await.is_err() {
                        // Peer closed; the per-connection loop will exit too.
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!(lagged = n, "plan relay: subscriber fell behind");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Build the [`StreamEvent`] reply for a [`ClientMessage::ResumeSession`].
///
/// The handler is intentionally read-only: it counts persisted message rows,
/// surfaces any supervisor-written [`ResumeToken`], and reports the high-water
/// turn so the client can decide whether to continue the session. The actual
/// in-memory `Session` re-spawn (re-running pending tool calls, attaching the
/// active provider, …) still belongs in the `Prompt` handler — `ResumeSession`
/// describes the persisted state without touching the live agent loop.
fn resume_session_event(session_store: &SessionStore, session_id: &str) -> StreamEvent {
    let messages = match session_store.load_messages(session_id) {
        Ok(m) => m,
        Err(e) => {
            return StreamEvent::AdminError {
                message: format!("load_messages({session_id}): {e}"),
            };
        }
    };
    if messages.is_empty() {
        return StreamEvent::AdminError {
            message: format!("unknown session: {session_id}"),
        };
    }
    let token = session_store.load_resume_token(session_id).unwrap_or(None);
    let had_resume_token = token.is_some();
    // Saturate at u32::MAX — a session with >4 G messages is not feasible.
    #[allow(clippy::cast_possible_truncation)]
    let messages_loaded = u32::try_from(messages.len()).unwrap_or(u32::MAX);
    let restored_to_turn = token
        .as_ref()
        .map_or_else(|| messages_loaded.saturating_sub(1), |t| t.last_turn);
    StreamEvent::SessionResumed {
        session_id: session_id.to_string(),
        messages_loaded,
        restored_to_turn,
        had_resume_token,
    }
}

/// Serialize `ev` and write it as a single `Event` frame. Mirrors the
/// pattern used by `handle_switch` for `StreamEvent::ProviderActive`,
/// but kept as a small helper because P13.2 emits three different
/// `StreamEvent` variants from the pair handler.
async fn write_event(conn: &SharedConnection, ev: &StreamEvent) -> Result<()> {
    let body = serde_json::to_vec(ev)?;
    conn.lock().await.write_frame(FrameKind::Event, &body).await?;
    Ok(())
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

/// Parse the optional `--metrics-bind <addr>` CLI flag, falling back to the
/// `ORIGIN_METRICS_BIND` env var.
///
/// We hand-roll the parser to avoid pulling in `clap` for a single flag.
fn parse_metrics_bind() -> Option<String> {
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--metrics-bind" {
            return args.next();
        }
        if let Some(rest) = a.strip_prefix("--metrics-bind=") {
            return Some(rest.to_string());
        }
    }
    env::var("ORIGIN_METRICS_BIND").ok()
}

/// Spawn a `hyper` 1.x `/metrics` server bound to `addr`.
///
/// Any request returns the current Prometheus text exposition; the body is
/// served as a single `Full<Bytes>` frame so the handler is allocation-only
/// per request. Bind failure is logged but does not abort the daemon.
fn spawn_metrics_endpoint(metrics: Metrics, addr: String) {
    spawn_in(TaskClass::Realtime, async move {
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => {
                warn!(error = %e, addr = %addr, "metrics: bind failed");
                return;
            }
        };
        info!(addr = %addr, "metrics: /metrics endpoint listening");
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(p) => p,
                Err(e) => {
                    warn!(error = %e, "metrics: accept failed");
                    continue;
                }
            };
            let metrics = metrics.clone();
            spawn_in(TaskClass::Realtime, async move {
                let io = hyper_util::rt::TokioIo::new(stream);
                let svc = hyper::service::service_fn(move |_req: hyper::Request<hyper::body::Incoming>| {
                    let body = metrics.encode_text().unwrap_or_default();
                    async move {
                        Ok::<_, std::convert::Infallible>(hyper::Response::new(http_body_util::Full::new(
                            hyper::body::Bytes::from(body),
                        )))
                    }
                });
                if let Err(e) = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await
                {
                    warn!(error = %e, "metrics: serve_connection error");
                }
            });
        }
    });
}
