// SPDX-License-Identifier: Apache-2.0
use std::env;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use origin_cas::Store;
use origin_codegraph::ask::NullMemRouter;
use origin_codegraph::index::CodeGraphIndex;
use origin_core::types::Role;
use origin_daemon::agent::{LoopOptions, SessionStoreSummaryDeliverer};
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
use origin_skills::SkillRegistry;
use origin_store::Store as SqlStore;
use origin_stream::Subscriber;
use origin_swarm::Coordinator;
use origin_tools::dispatch::MemoryHandle as MemoryHandleTrait;
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
    // Before anything else: check for the `install-ra` subcommand.
    // This runs synchronously and exits without starting the full daemon.
    {
        let mut args = std::env::args().skip(1).peekable();
        if args.peek().is_some_and(|a| a == "install-ra") {
            return install_ra();
        }
    }

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
            // tracing::error! lands in the parquet trace ring, which is great
            // for postmortems but useless when the daemon is failing during
            // boot under auto-spawn (no console attached, no chance for the
            // operator to see it). Mirror the error to stderr so the parent
            // sees *something* before exit code 0.
            eprintln!("origin-daemon: daemon_setup terminated with error: {e}");
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
    // N4.3 shared handle→band index. One instance for the daemon's
    // lifetime: cloned into the provider factory (so each `Anthropic` build
    // gets `with_plan(plan.clone())`) and into per-request `LoopOptions`
    // (so the tool-result dispatch site registers each freshly produced
    // CAS handle). All clones share the same inner `Arc<RwLock<…>>`, so
    // registrations from the writer side are immediately visible to the
    // wire-encoder reader. Map size grows roughly linearly in unique tool
    // results across the daemon's run; per-entry cost is ~33 bytes.
    let wire_plan: origin_planner::Plan = origin_planner::Plan::default();
    let factory = ProviderFactory::new(vault.clone(), catalog)
        .with_cas(Arc::clone(&cas))
        .with_plan(wire_plan.clone());

    let initial_account = env::var("ORIGIN_ACCOUNT").unwrap_or_else(|_| "default".into());
    // Register the factory process-wide so the agent loop can rebuild a provider
    // mid-loop for a CROSS-PROVIDER router pick (foundation L84 / kilo L265). This
    // is inert unless `ORIGIN_ROUTER` is set and a pick lands on a different
    // provider; credentials still resolve only through this factory's vault.
    origin_daemon::provider_factory::set_global(
        Arc::new(factory.clone()),
        initial_account.clone(),
    );
    let initial_provider_str = if let Ok(v) = env::var("ORIGIN_PROVIDER") {
        v
    } else {
        // Auto-detect: prefer anthropic-oauth when OAuth tokens exist in
        // the vault but no raw API key is stored.
        let has_api_key = vault.get("anthropic", &initial_account).await.is_ok();
        let has_oauth = vault
            .get("anthropic-oauth", &format!("{initial_account}/oauth"))
            .await
            .is_ok();
        if !has_api_key && has_oauth {
            info!("no anthropic API key found; using anthropic-oauth");
            "anthropic-oauth".into()
        } else {
            "anthropic".into()
        }
    };
    let initial_provider_id = ProviderId::parse(&initial_provider_str, factory.catalog())
        .ok_or_else(|| anyhow::anyhow!("ORIGIN_PROVIDER `{initial_provider_str}` is not a known provider"))?;

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

    // Code-graph subsystem (P7.8 Subsystem A). Opens its own CAS root and
    // SQL connection so the index never contends with the session store or
    // memory subsystem. Graceful-degrade: log a warning and leave the
    // optional `None` if either store fails to open.
    let codegraph_cas_root = format!("{cas_root}/codegraph");
    let code_graph: Arc<tokio::sync::Mutex<CodeGraphIndex>> = {
        let cg_cas = origin_cas::Store::open(origin_cas::StoreConfig {
            root: codegraph_cas_root.clone().into(),
            hot_capacity: 128,
            warm_pack_target_bytes: 4 * 1024 * 1024,
            cold_zstd_level: 3,
        });
        let cg_sql = SqlStore::open(&db_path);
        match (cg_cas, cg_sql) {
            (Ok(cas_store), Ok(sql_store)) => {
                info!("code-graph index ready");
                Arc::new(tokio::sync::Mutex::new(CodeGraphIndex::new(cas_store, sql_store)))
            }
            (Err(e), _) => {
                warn!(error = %e, "code-graph: CAS open failed; graph tools disabled");
                // Build an empty fallback so the Arc type is satisfied.
                // The stores will be opened in a fallback temp dir.
                let tmp = std::env::temp_dir().join("origin-cg-fallback");
                let _ = std::fs::create_dir_all(&tmp);
                let fallback_cas = origin_cas::Store::open(origin_cas::StoreConfig {
                    root: tmp,
                    hot_capacity: 8,
                    warm_pack_target_bytes: 1 << 20,
                    cold_zstd_level: 1,
                })
                .expect("fallback codegraph cas");
                let fallback_sql = SqlStore::open(&db_path).expect("fallback codegraph sql");
                Arc::new(tokio::sync::Mutex::new(CodeGraphIndex::new(
                    fallback_cas,
                    fallback_sql,
                )))
            }
            (_, Err(e)) => {
                warn!(error = %e, "code-graph: SQL open failed; graph tools disabled");
                let tmp = std::env::temp_dir().join("origin-cg-fallback");
                let _ = std::fs::create_dir_all(&tmp);
                let fallback_cas = origin_cas::Store::open(origin_cas::StoreConfig {
                    root: tmp,
                    hot_capacity: 8,
                    warm_pack_target_bytes: 1 << 20,
                    cold_zstd_level: 1,
                })
                .expect("fallback codegraph cas");
                let fallback_sql = SqlStore::open(&db_path).expect("fallback codegraph sql");
                Arc::new(tokio::sync::Mutex::new(CodeGraphIndex::new(
                    fallback_cas,
                    fallback_sql,
                )))
            }
        }
    };
    let mem_router: Arc<dyn origin_codegraph::ask::MemRouter> = Arc::new(NullMemRouter);

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
        // Bridge the per-handle broadcast into the daemon-wide PlanBus so
        // `ClientMessage::SubscribePlan` subscribers actually see plan ops.
        // Without this bridge the subscribe path is silently empty.
        {
            let mut rx = plan_handle.subscribe();
            let bus = plan_bus.clone();
            spawn_in(TaskClass::Realtime, async move {
                loop {
                    match rx.recv().await {
                        Ok(env) => bus.publish(env),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!(lagged = n, "plan bridge: fell behind PlanHandle broadcast");
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }
        // Install the REAL agent-loop worker (replacing the noop). Each `Task`
        // dispatch now runs a bounded sub-agent against a snapshot of the active
        // provider, with its tools narrowed to the worker's allow-list (minus
        // `Task`). Worker bodies run in `TaskClass::Sidecar` (see Coordinator),
        // so a parent awaiting a child never deadlocks the Critical pool.
        let mut coord = Coordinator::new(plan_handle, "origin-daemon");
        coord.set_default_worker(origin_daemon::swarm_worker::real_worker(Arc::clone(&active)));
        Arc::new(coord)
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

    // Workflows catalog: loaded once at startup so every turn's system prompt
    // can advertise them. Re-load on user edits happens via `ActivateWorkflow`'s
    // existing on-demand load path; this snapshot is for advertising only.
    let workflows_catalog: Arc<origin_daemon::workflows::WorkflowsFile> = {
        let home = std::env::var_os("ORIGIN_HOME")
            .map(std::path::PathBuf::from)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let path = home.join(".origin").join("workflows.toml");
        match origin_daemon::workflows::load_from(&path) {
            Ok(f) => Arc::new(f),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "workflows.toml load failed; running with empty workflows catalog");
                Arc::new(origin_daemon::workflows::WorkflowsFile::default())
            }
        }
    };
    info!(
        workflow_count = workflows_catalog.workflows.len(),
        "workflows catalog loaded at startup"
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
    // cline/gemini OpenTelemetry export: when built `--features otel` and
    // ORIGIN_OTLP_ENDPOINT is set, install a real OTLP/gRPC metrics pipeline.
    // The global meter provider keeps the returned handle alive (we drop the
    // local clone). Off by default (feature + env both required).
    #[cfg(feature = "otel")]
    {
        if let Ok(endpoint) = env::var("ORIGIN_OTLP_ENDPOINT") {
            match origin_metrics::exporter::otel::install(&endpoint) {
                Ok(_provider) => info!(%endpoint, "otel: OTLP metrics exporter installed"),
                Err(e) => tracing::warn!(error = %e, "otel: failed to install OTLP exporter"),
            }
        }
    }

    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let listener = Listener::bind(&path).await?;
    info!(path = %path, "origin-daemon listening");

    // Default-off autonomous background loops (items J + K). Each is gated on
    // its own env var (`ORIGIN_SCHEDULER=1` / `ORIGIN_AMBIENT=1`); when unset
    // these spawn nothing. When set, a fired trigger / selected ambient task
    // connects back to the socket we just bound and submits a real `Prompt`,
    // so it runs through the exact same agent path as an interactive turn.
    origin_daemon::scheduler::maybe_spawn(path.clone());
    origin_daemon::ambient::maybe_spawn(path.clone());
    // jcode Overnight mode (ORIGIN_OVERNIGHT=1); off by default. Runs an
    // OvernightPlan to completion within a wall-clock window and persists a
    // morning report to ~/.origin/overnight/ for `origin ambient report`.
    origin_daemon::overnight::maybe_spawn(path.clone());
    // Authenticated HTTP webhook trigger source (ORIGIN_WEBHOOK + _TOKEN); off
    // by default. A POST fires its body as a prompt onto the live agent path.
    origin_daemon::webhook::maybe_spawn(path.clone());
    // gemini-cli Auto Memory (ORIGIN_MEM_GARDEN=1); off by default. Mines recent
    // session transcripts on an idle cadence into a secret-redacted review inbox
    // at ~/.origin/memory-inbox/ for the user to accept/reject.
    origin_daemon::mem_garden::maybe_spawn(Arc::clone(&session_store));

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
            Arc::clone(&workflows_catalog),
            Arc::clone(&code_graph),
            Arc::clone(&mem_router),
            Arc::clone(&coordinator),
            wire_plan.clone(),
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
    workflows_catalog: Arc<origin_daemon::workflows::WorkflowsFile>,
    code_graph: Arc<tokio::sync::Mutex<CodeGraphIndex>>,
    mem_router: Arc<dyn origin_codegraph::ask::MemRouter>,
    coordinator: Arc<Coordinator>,
    wire_plan: origin_planner::Plan,
) {
    // Build a type-erased memory handle once per connection so `handle_request`
    // doesn't need to know about `MemoryWiring` internals.
    let memory_handle: Option<Arc<dyn MemoryHandleTrait>> =
        memory.as_ref().map(|m| m.handle() as Arc<dyn MemoryHandleTrait>);
    spawn_in(TaskClass::Critical, async move {
        // Per-connection skill activation state. Each ActivateSkill mutates
        // this registry; each Prompt reads its `allowed_tools` mask and passes
        // it through LoopOptions.skills so the permission engine narrows
        // accordingly. Wrapped in Arc<Mutex<...>> so we can hand `Arc::clone`s
        // to async handlers without giving up the registry.
        let active_skills: Arc<tokio::sync::Mutex<SkillRegistry>> =
            Arc::new(tokio::sync::Mutex::new(SkillRegistry::new()));
        // Per-connection workflow progress. Populated by
        // `ActivateWorkflow` with the first resolvable step; advanced
        // by `advance_workflow` after each successful `Prompt`.
        let active_workflow: Arc<
            tokio::sync::Mutex<Option<origin_daemon::workflow_progress::WorkflowProgress>>,
        > = Arc::new(tokio::sync::Mutex::new(None));
        // Per-connection `/goal` slot. `Some` while a goal is active; the
        // post-`run_loop` driver in `drive_goal_loop` reads/mutates it and
        // emits `Goal*` events back through the connection.
        let active_goal: Arc<tokio::sync::Mutex<Option<origin_goal::GoalState>>> =
            Arc::new(tokio::sync::Mutex::new(None));
        // Per-connection push-back slot. `drive_goal_loop` decodes any
        // frame it peeks mid-iteration; if the frame is a non-Interrupt
        // `ClientMessage` (a follow-up `Prompt`, an admin call, …) the
        // driver stashes it here and bails out, so the outer loop picks it
        // up on its NEXT iteration instead of reading a fresh frame off
        // the wire. This preserves the user's input that the previous
        // peek-and-drop implementation silently discarded.
        let pending_message: Arc<tokio::sync::Mutex<Option<ClientMessage>>> =
            Arc::new(tokio::sync::Mutex::new(None));
        // Bug #8: the most recent session id seen on this connection. The
        // Prompt handler updates it after binding a session; the `/goal`
        // activation handler reads it so it can write a checkpoint
        // immediately on activation rather than waiting for the first
        // iteration to complete (which is the window a crash could lose
        // the goal in).
        let last_known_session_id: Arc<tokio::sync::Mutex<Option<String>>> =
            Arc::new(tokio::sync::Mutex::new(None));

        loop {
            // Step 1: drain any pushed-back message left by the goal driver
            // on the previous iteration. When `Some`, skip the wire read
            // entirely and dispatch the buffered message directly.
            let pushed_back: Option<ClientMessage> = pending_message.lock().await.take();
            let msg: ClientMessage = if let Some(m) = pushed_back {
                m
            } else {
                let body = {
                    let mut g = conn.lock().await;
                    match g.read_frame_body().await {
                        Ok(b) => b,
                        Err(_) => break,
                    }
                };
                // Try the new ClientMessage envelope first, then fall back
                // to the legacy raw `PromptRequest` shape (back-compat for
                // clients that pre-date P6.7).
                if let Ok(m) = serde_json::from_slice::<ClientMessage>(&body) {
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
                    // Build a Haiku-backed verifier per prompt so it always
                    // reflects the current provider snapshot. The verifier is
                    // only invoked when a goal is active AND the main model
                    // claimed `met`, so per-prompt construction is cheap
                    // (allocation only; no network calls here).
                    let verifier: Arc<dyn origin_goal::verifier::Verifier> =
                        Arc::new(origin_daemon::anthropic_verifier::AnthropicHaikuVerifier {
                            provider: Arc::clone(&provider_snapshot),
                            model: "claude-haiku-4-5".to_string(),
                        });
                    let outcome = handle_request(
                        &conn,
                        provider_snapshot.as_ref(),
                        Arc::clone(&session_store),
                        Arc::clone(&cas),
                        Arc::clone(&sidecar),
                        memory.as_ref(),
                        memory_handle.clone(),
                        Arc::clone(&proposal_registry),
                        Arc::clone(&skill_catalog),
                        Arc::clone(&workflows_catalog),
                        Arc::clone(&active_skills),
                        Arc::clone(&code_graph),
                        Arc::clone(&mem_router),
                        Arc::clone(&coordinator),
                        wire_plan.clone(),
                        Arc::clone(&active_goal),
                        Arc::clone(&pending_message),
                        Arc::clone(&last_known_session_id),
                        verifier,
                        req,
                    )
                    .await;
                    match outcome {
                        PromptOutcome::ConnectionDead => break,
                        PromptOutcome::Succeeded => {
                            // Gate cleared: advance the workflow one step.
                            advance_workflow(
                                &conn,
                                Arc::clone(&active_workflow),
                                Arc::clone(&active_skills),
                                Arc::clone(&skill_catalog),
                            )
                            .await;
                        }
                        PromptOutcome::Failed { message } => {
                            // Halt-on-error: workflow stays paused at the
                            // same step. Next successful Prompt resumes.
                            hold_workflow(&conn, Arc::clone(&active_workflow), &message).await;
                        }
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
                    handle_resume_request(&conn, Arc::clone(&session_store), Arc::clone(&active_goal), token)
                        .await;
                }
                ClientMessage::ActivateSkill { name, args } => {
                    let conn_clone = Arc::clone(&conn);
                    // `/goal` has dedicated routing — it does NOT push onto the
                    // skill stack like a normal skill. Bare `/goal` is a status
                    // query; `/goal <cond>` activates (replacing any prior goal).
                    if name == "goal" {
                        handle_goal_activation(
                            &conn_clone,
                            Arc::clone(&active_goal),
                            Arc::clone(&session_store),
                            Arc::clone(&last_known_session_id),
                            args.as_deref(),
                        )
                        .await;
                        continue;
                    }
                    // Look up the skill in the daemon-wide catalog loaded at
                    // startup. The catalog is the single source of truth shared
                    // with the system-prompt injection (see agent.rs::run_loop).
                    if let Some(skill) = skill_catalog.find(&name) {
                        let front = skill.front.clone();
                        let allowed_tools: Vec<String> = {
                            let mut guard = active_skills.lock().await;
                            guard.activate(front);
                            // After activation, the intersection always exists (we just pushed a skill).
                            // Sort for stable wire output so clients see a deterministic order.
                            guard
                                .allowed_tools()
                                .map(|set| {
                                    let mut v: Vec<String> = set.into_iter().collect();
                                    v.sort();
                                    v
                                })
                                .unwrap_or_default()
                        };
                        let ev = StreamEvent::SkillActive {
                            name: name.clone(),
                            allowed_tools,
                        };
                        let body = serde_json::to_vec(&ev).unwrap_or_default();
                        let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
                    } else {
                        let ev = StreamEvent::SkillError {
                            message: format!("no such skill: {name}"),
                        };
                        let body = serde_json::to_vec(&ev).unwrap_or_default();
                        let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
                    }
                }
                ClientMessage::DeactivateSkill { name } => {
                    // `/-goal` clears the per-connection goal slot. Other
                    // names go to the skill stack as before.
                    if name == "goal" {
                        let mut slot = active_goal.lock().await;
                        if let Some(prior) = slot.take() {
                            let ev = StreamEvent::GoalCleared {
                                reason: origin_goal::ClearReasonWire::UserSlash,
                                iter: prior.iter,
                                tokens_spent: prior.tokens_spent,
                            };
                            drop(slot);
                            let body = serde_json::to_vec(&ev).unwrap_or_default();
                            let _ = conn.lock().await.write_frame(FrameKind::Event, &body).await;
                        } else {
                            let body = serde_json::to_vec(&StreamEvent::AdminOk).unwrap_or_default();
                            let _ = conn.lock().await.write_frame(FrameKind::Event, &body).await;
                        }
                        continue;
                    }
                    active_skills.lock().await.deactivate(&name);
                    let body = serde_json::to_vec(&StreamEvent::AdminOk).unwrap_or_default();
                    let _ = conn.lock().await.write_frame(FrameKind::Event, &body).await;
                }
                ClientMessage::ActivateWorkflow { name } => {
                    use origin_daemon::workflow_progress::{StartOutcome, WorkflowProgress};
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
                    // Defensive: if a prior workflow's step is still active
                    // on this connection, drop its skill before starting a
                    // new one. (User invoked /workflow before the previous
                    // one ran to completion.)
                    {
                        let mut wf_guard = active_workflow.lock().await;
                        if let Some(prev) = wf_guard.take() {
                            active_skills.lock().await.deactivate(&prev.current_skill);
                        }
                    }
                    let ev = match WorkflowProgress::start(wf, skill_catalog.as_ref()) {
                        StartOutcome::Stepped {
                            progress,
                            front,
                            skipped,
                        } => {
                            active_skills.lock().await.activate(front);
                            let step_index = u32::try_from(progress.current_step_index).unwrap_or(u32::MAX);
                            let total_steps = u32::try_from(progress.total_steps).unwrap_or(u32::MAX);
                            let skill = progress.current_skill.clone();
                            *active_workflow.lock().await = Some(progress);
                            StreamEvent::WorkflowStepActive {
                                name: name.clone(),
                                step_index,
                                total_steps,
                                skill,
                                skipped,
                            }
                        }
                        StartOutcome::NoResolvableSteps { skipped } => StreamEvent::WorkflowActive {
                            name: name.clone(),
                            steps: Vec::new(),
                            skipped,
                        },
                    };
                    let body = serde_json::to_vec(&ev).unwrap_or_default();
                    let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
                }
                ClientMessage::SubscribePlan => {
                    spawn_plan_relay(plan_bus.subscribe(), Arc::clone(&conn));
                }
                ClientMessage::Interrupt => {
                    // When `Interrupt` lands in the OUTER loop it means
                    // the previous `Prompt` already finished (or there
                    // was no in-flight prompt to begin with). The driver's
                    // mid-iteration push-back path catches the more common
                    // case where `Interrupt` arrives DURING a goal
                    // iteration — see `drive_goal_loop`.
                    //
                    // If a goal is still active here it means a previous
                    // iteration completed before the user's interrupt
                    // was processed; clear it now with `UserSlash` so the
                    // CLI sees the same terminal event regardless of
                    // race timing. No-op when no goal is active.
                    let mut slot = active_goal.lock().await;
                    if let Some(prior) = slot.take() {
                        let ev = StreamEvent::GoalCleared {
                            reason: origin_goal::ClearReasonWire::UserSlash,
                            iter: prior.iter,
                            tokens_spent: prior.tokens_spent,
                        };
                        drop(slot);
                        // Bug #17: write a terminal-status checkpoint so a
                        // crash between cancellation and the next user
                        // Prompt cannot resurrect a goal the user just
                        // killed.
                        let sid_opt = last_known_session_id.lock().await.clone();
                        if let Some(sid) = sid_opt {
                            let snap = origin_goal::GoalSnapshot {
                                condition: prior.condition.clone(),
                                iter: prior.iter,
                                max_iter: prior.max_iter,
                                tokens_spent: prior.tokens_spent,
                                token_budget: prior.token_budget,
                                started_at_unix: prior
                                    .started_at
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0),
                                status: origin_goal::GoalStatusWire::Cleared {
                                    by: origin_goal::ClearReasonWire::UserSlash,
                                },
                                last_status_tag: prior.last_status_tag.clone().map(Into::into),
                            };
                            let token = origin_resume_token::ResumeToken {
                                session_id: sid,
                                last_turn: 0,
                                cas_handle_root: [0u8; 32],
                                pending_tool_calls: Vec::new(),
                                plan_seq: 0,
                                goal: Some(snap),
                                detached_at_unix: None,
                                memory_estimate_bytes: None,
                            };
                            if let Err(e) = session_store.save_resume_token(&token) {
                                warn!(error = %e, "goal interrupt: terminal save failed");
                            }
                        }
                        let _ = write_event(&conn, &ev).await;
                    }
                }
                ClientMessage::ClearAll => {
                    // `/clear` is mechanical: it resets the in-session context
                    // without ever touching the skill stack or catalog. Its
                    // only stateful effect is terminating any active goal
                    // (bug #10), after which it acks with AdminOk.
                    handle_clear_all(
                        &conn,
                        Arc::clone(&active_goal),
                        Arc::clone(&session_store),
                        Arc::clone(&last_known_session_id),
                    )
                    .await;
                }
                admin @ (ClientMessage::ListSessions
                | ClientMessage::RemoveSession { .. }
                | ClientMessage::RewindSession { .. }
                | ClientMessage::ResumeSession { .. }
                | ClientMessage::ResumeForeign { .. }
                | ClientMessage::ExportSession { .. }
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
    active_goal: Arc<tokio::sync::Mutex<Option<origin_goal::GoalState>>>,
    token: origin_resume_token::ResumeToken,
) {
    let session_id = token.session_id.clone();
    if let Err(e) = session_store.save_resume_token(&token) {
        warn!(error = %e, session = %session_id, "resume: could not persist token");
    }
    // Hydrate a previously-active goal back into the per-connection slot.
    // Only `Active` or `Verifying` snapshots are restored — terminal
    // statuses (`Met`/`Cleared`) carry their own outcomes that the CLI
    // already saw, so re-installing them would be confusing. The user's
    // next `Prompt` resumes the driver loop; we deliberately do NOT
    // auto-iterate on resume per §5 of the spec.
    if let Some(snapshot) = token.goal.as_ref() {
        use origin_goal::{GoalSnapshot, GoalStatusWire, TagOutcome, TagOutcomeWire};
        let GoalSnapshot {
            condition,
            iter,
            max_iter,
            tokens_spent,
            token_budget,
            started_at_unix,
            status,
            last_status_tag,
        } = snapshot.clone();
        if matches!(status, GoalStatusWire::Active | GoalStatusWire::Verifying) {
            let started_at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(started_at_unix);
            // Bug #9: preserve `last_status_tag` across resume so a
            // `Verifying` resume gets a fresh verifier call on the next
            // tick (the driver dispatches on `Met` and re-invokes the
            // verifier rather than blindly clearing).
            let restored_tag: Option<TagOutcome> = last_status_tag.map(|w| match w {
                TagOutcomeWire::Met => TagOutcome::Met,
                TagOutcomeWire::InProgress { what_remains } => TagOutcome::InProgress { what_remains },
                TagOutcomeWire::Blocked { why } => TagOutcome::Blocked { why },
                TagOutcomeWire::Missing => TagOutcome::Missing,
            });
            let restored = origin_goal::GoalState {
                condition: condition.clone(),
                status: origin_goal::GoalStatus::Active,
                iter,
                max_iter,
                tokens_spent,
                token_budget,
                started_at,
                // Bug #25: monotonic counterpart unknown on resume; the
                // wall-clock origin in `started_at` is what we have.
                // `Instant::now()` is the safe default — any elapsed-since-
                // start math computed from this Instant will measure time
                // since RESUME, not since original goal activation.
                started_at_instant: std::time::Instant::now(),
                last_status_tag: restored_tag,
                consecutive_rejections: 0,
            };
            *active_goal.lock().await = Some(restored);
            let ev = StreamEvent::GoalActive {
                condition,
                max_iter,
                token_budget,
            };
            let body = serde_json::to_vec(&ev).unwrap_or_default();
            let _ = conn.lock().await.write_frame(FrameKind::Event, &body).await;
        }
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
/// Outcome of a single `Prompt` dispatch.
///
/// The dispatcher branches on this to decide whether to advance the
/// per-connection workflow (`Succeeded`), hold it on the same step
/// (`Failed`), or tear down the connection (`ConnectionDead`).
#[derive(Debug)]
enum PromptOutcome {
    Succeeded,
    Failed { message: String },
    ConnectionDead,
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn handle_request(
    conn: &SharedConnection,
    provider: &dyn Provider,
    session_store: Arc<SessionStore>,
    cas: Arc<Store>,
    sidecar: Arc<Sidecar>,
    memory: Option<&MemoryWiring>,
    memory_handle: Option<Arc<dyn MemoryHandleTrait>>,
    proposal_registry: Arc<ProposalRegistry>,
    skill_catalog: Arc<SkillCatalog>,
    workflows_catalog: Arc<origin_daemon::workflows::WorkflowsFile>,
    active_skills: Arc<tokio::sync::Mutex<SkillRegistry>>,
    code_graph: Arc<tokio::sync::Mutex<CodeGraphIndex>>,
    mem_router: Arc<dyn origin_codegraph::ask::MemRouter>,
    coordinator: Arc<Coordinator>,
    plan: origin_planner::Plan,
    active_goal: Arc<tokio::sync::Mutex<Option<origin_goal::GoalState>>>,
    pending_message: Arc<tokio::sync::Mutex<Option<ClientMessage>>>,
    last_known_session_id: Arc<tokio::sync::Mutex<Option<String>>>,
    verifier: Arc<dyn origin_goal::verifier::Verifier>,
    req: PromptRequest,
) -> PromptOutcome {
    let mut session = if let Some(sid) = &req.session_id {
        match session_store.load_messages(sid) {
            Ok(msgs) if !msgs.is_empty() => {
                let mut s = Session::new_with_id(sid.clone(), req.model.clone());
                s.provider_name = provider.name().to_string();
                for m in msgs {
                    s.push(m);
                }
                s
            }
            _ => {
                let mut s = Session::new(provider.name(), &req.model);
                // `clone_from` reuses the existing `id` allocation instead of
                // dropping it and allocating a fresh `String` (clippy
                // `assigning_clones`).
                s.id.clone_from(sid);
                s
            }
        }
    } else {
        Session::new(provider.name(), &req.model)
    };
    // cline multi-root: surface any extra workspace roots to the agent loop,
    // which renders them as a `<workspace-roots>` block. Empty ⇒ no change.
    if !req.roots.is_empty() {
        session.roots = req.roots.iter().map(std::path::PathBuf::from).collect();
    }
    // Bug #8: stash the session id so a later `/goal` activation on this
    // connection can checkpoint without waiting for the first iteration
    // to complete.
    *last_known_session_id.lock().await = Some(session.id.clone());
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
            max_turns: 200,
            cas: Some(cas),
            code_graph: Some(Arc::clone(&code_graph)),
            mem_router: Some(Arc::clone(&mem_router)),
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
                let snapshot_opt: Option<SkillRegistry> = if guard.allowed_tools().is_some() {
                    let mut snapshot = SkillRegistry::new();
                    for s in guard.iter_active() {
                        snapshot.activate(s.clone());
                    }
                    Some(snapshot)
                } else {
                    None
                };
                drop(guard);
                snapshot_opt.map(Arc::new)
            },
            skill_catalog: Some(Arc::clone(&skill_catalog)),
            workflows: Some(Arc::clone(&workflows_catalog)),
            memory_handle: memory_handle.clone(),
            coordinator: Some(Arc::clone(&coordinator)),
            plan: Some(plan.clone()),
            // ^ shared with the Anthropic provider's wire-encoder via the
            // `Arc<RwLock<…>>` inside `Plan`. The dispatch loop registers
            // every produced CAS handle (Sticky for Pure tools, Volatile
            // for Mutating) so the encoder can downgrade `Inline` to
            // `Reference` on subsequent turns.
            goal: Arc::clone(&active_goal),
            // ^ per-connection goal slot. When `Some(Active|Verifying)` the
            // `run_loop` body renders an `<origin-goal>` block on each turn;
            // the post-loop driver below decides verify-vs-iterate-vs-clear.
            policy: None,
            conseca: None,
            // ^ DENY-ONLY governance overlay (Task 3). Wired as fields but left
            // `None` here so default daemon behavior is unchanged; a future
            // admin-config path can populate these to narrow tool access.
            effort: req
                .effort
                .as_deref()
                .and_then(origin_provider::ReasoningEffort::from_wire_str),
            // ^ claude-code `/effort`+`/fast`: the CLI sends a canonical token;
            // an unknown token maps to `None` ⇒ wire byte-identical.
            thinking_tokens: req.thinking_tokens,
            // ^ aider `--thinking-tokens`: only the Anthropic encoder honours it
            // (extended thinking with `budget_tokens`); `None` ⇒ wire unchanged.
            attachments: req.attachments.clone(),
            // ^ aider/gemini/claude image+PDF input: applied to turn 1 only.
            system_suffix: (!req.system.is_empty()).then(|| req.system.clone()),
            // ^ claude-code output styles: the CLI puts the active style's
            // system suffix in `req.system`; empty ⇒ no addendum (wire unchanged).
            read_only: req.read_only,
            // ^ gemini Plan Mode: deny-only read-only overlay for this turn.
            router: origin_daemon::routing::global(),
            // ^ aider/gemini/kilo/openclaude live model routing: process-wide
            // router built once from `ORIGIN_ROUTER` (unset ⇒ None ⇒ each turn
            // uses session.model, byte-identical). Per-turn phase selection +
            // health/quota feedback live inside `run_loop`.
        };
        drive_goal_loop(
            conn,
            &mut session,
            req.user_text.clone(),
            provider,
            &opts,
            Arc::clone(&active_goal),
            Arc::clone(&pending_message),
            Arc::clone(&session_store),
            verifier.as_ref(),
            event_tx.clone(),
        )
        .await
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
                    return PromptOutcome::ConnectionDead;
                }
            }
            // Persist first so the rows exist before the sidecar deliverer
            // fires update_summary.
            persist(session_store.as_ref(), &session);
            // Submit one Summarize job per assistant turn (P5.2, N2.5.a).
            submit_summarize_jobs(&sidecar, &session_store, &session);
            PromptOutcome::Succeeded
        }
        Err(e) => {
            let message = format!("loop error: {e}");
            let _ = conn
                .lock()
                .await
                .write_frame(FrameKind::ErrorFrame, message.as_bytes())
                .await;
            PromptOutcome::Failed { message }
        }
    }
}

/// Handle `/goal [<args>]` (the dedicated `ActivateSkill { name: "goal", ... }`
/// path). Bare `/goal` queries the active goal; `/goal <cond>` parses + activates
/// (replacing any prior goal). All wire writes go through `conn`.
async fn handle_goal_activation(
    conn: &SharedConnection,
    active_goal: Arc<tokio::sync::Mutex<Option<origin_goal::GoalState>>>,
    session_store: Arc<SessionStore>,
    last_known_session_id: Arc<tokio::sync::Mutex<Option<String>>>,
    raw_args: Option<&str>,
) {
    let Some(raw) = raw_args else {
        // Bare `/goal` — status query. With no goal we emit the benign
        // `GoalInactive` event so the CLI renders it as an info line, not
        // an error (bug #20).
        //
        // Narrow the lock: copy out exactly the fields the event needs while
        // the guard is held, then release it at the end of this statement —
        // before the event is built — so no lock guard sits in the `match`
        // scrutinee (clippy `significant_drop_in_scrutinee` /
        // `option_if_let_else`). The guard temporary lives only for this `let`.
        let active_fields: Option<(String, u32, u64)> = active_goal
            .lock()
            .await
            .as_ref()
            .map(|g| (g.condition.clone(), g.max_iter, g.token_budget));
        let ev = active_fields.map_or(
            StreamEvent::GoalInactive,
            |(condition, max_iter, token_budget)| StreamEvent::GoalActive {
                condition,
                max_iter,
                token_budget,
            },
        );
        let body = serde_json::to_vec(&ev).unwrap_or_default();
        let _ = conn.lock().await.write_frame(FrameKind::Event, &body).await;
        return;
    };
    match origin_goal::parse_goal_args(raw) {
        Ok(parsed) => {
            // Replace any prior goal, emitting `GoalCleared { UserSlash }` so
            // the CLI sees the old goal end before the new one starts.
            let mut slot = active_goal.lock().await;
            if let Some(prior) = slot.take() {
                let ev = StreamEvent::GoalCleared {
                    reason: origin_goal::ClearReasonWire::UserSlash,
                    iter: prior.iter,
                    tokens_spent: prior.tokens_spent,
                };
                let body = serde_json::to_vec(&ev).unwrap_or_default();
                let _ = conn.lock().await.write_frame(FrameKind::Event, &body).await;
            }
            let new_goal =
                origin_goal::GoalState::new(parsed.condition.clone(), parsed.max_iter, parsed.token_budget);
            let active = StreamEvent::GoalActive {
                condition: new_goal.condition.clone(),
                max_iter: new_goal.max_iter,
                token_budget: new_goal.token_budget,
            };
            *slot = Some(new_goal);
            drop(slot);
            let body = serde_json::to_vec(&active).unwrap_or_default();
            let _ = conn.lock().await.write_frame(FrameKind::Event, &body).await;
            // Bug #8: persist the activation immediately so a crash
            // between `/goal foo` and the first iteration cannot lose the
            // goal. We need a session_id; use the most recent one this
            // connection's Prompt handler stashed. If activation happened
            // BEFORE any Prompt (no session bound yet), we log a warn and
            // skip the checkpoint — the limitation is documented.
            let sid_opt = last_known_session_id.lock().await.clone();
            if let Some(sid) = sid_opt {
                use origin_daemon::goal_checkpoint::make_goal_checkpoint_token;
                let token = {
                    let guard = active_goal.lock().await;
                    make_goal_checkpoint_token(&sid, 0, &guard)
                };
                if let Err(e) = session_store.save_resume_token(&token) {
                    warn!(error = %e, "goal activation: checkpoint save failed");
                }
            } else {
                warn!(
                    "goal activation: no session bound to this connection yet; \
                     skipping immediate checkpoint. A crash before the first \
                     Prompt will lose this goal."
                );
            }
        }
        Err(e) => {
            let ev = StreamEvent::SkillError {
                message: format!("/goal: {e}"),
            };
            let body = serde_json::to_vec(&ev).unwrap_or_default();
            let _ = conn.lock().await.write_frame(FrameKind::Event, &body).await;
        }
    }
}

/// Handle the mechanical `/clear` admin verb ([`ClientMessage::ClearAll`]).
///
/// `/clear` is a first-class context reset, not a skill: it never touches the
/// per-connection skill stack or the skill catalog. Its only stateful effect is
/// terminating any active goal so the next `Prompt` cannot silently resume the
/// driver loop (bug #10). The sequence is:
///
/// 1. Take the active-goal slot. If a goal was running, write the terminal
///    `Cleared { UserClearAll }` checkpoint (so a crash between `/clear` and
///    the next `Prompt` cannot resurrect it) and emit
///    [`StreamEvent::GoalCleared`].
/// 2. Always finish with [`StreamEvent::AdminOk`] so the CLI sees a terminal
///    ack for the request — whether or not a goal was cleared.
async fn handle_clear_all(
    conn: &SharedConnection,
    active_goal: Arc<tokio::sync::Mutex<Option<origin_goal::GoalState>>>,
    session_store: Arc<SessionStore>,
    last_known_session_id: Arc<tokio::sync::Mutex<Option<String>>>,
) {
    let prior_opt = {
        let mut slot = active_goal.lock().await;
        slot.take()
    };
    if let Some(prior) = prior_opt {
        if let Some(ev) = origin_daemon::goal_clear_all::clear_all_event_for(Some(&prior)) {
            // Terminal-status checkpoint so a crash between /clear and the next
            // Prompt cannot resurrect the goal the user just discarded. Mirrors
            // the Interrupt arm's bug-#17 fix.
            let sid_opt = last_known_session_id.lock().await.clone();
            if let Some(sid) = sid_opt {
                let token = cleared_resume_token(&sid, 0, &prior, origin_goal::ClearReasonWire::UserClearAll);
                if let Err(e) = session_store.save_resume_token(&token) {
                    warn!(error = %e, "clear: terminal goal checkpoint save failed");
                }
            }
            let _ = write_event(conn, &ev).await;
        }
    }
    let _ = write_event(conn, &StreamEvent::AdminOk).await;
}

/// Pull the last assistant text out of `session` for tag parsing + verifier
/// input. Concatenates every `Block::Text` body of the most recent
/// `Role::Assistant` message; returns empty when none exists (shouldn't
/// happen after a successful `run_loop` but the empty fallback keeps the
/// driver from panicking).
fn last_assistant_text(session: &Session) -> String {
    use origin_core::types::{Block, Role};
    for msg in session.messages.iter().rev() {
        if matches!(msg.role, Role::Assistant) {
            return msg
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<String>();
        }
    }
    String::new()
}

/// The empty `LoopSummary` returned when a goal iteration clears before any
/// `run_loop` summary is available. Extracted so the return sites that need it
/// stay identical (`LoopSummary` does not derive `Default`). `const` to satisfy
/// clippy `missing_const_for_fn` (every field initializer is const).
const fn empty_loop_summary() -> origin_daemon::agent::LoopSummary {
    origin_daemon::agent::LoopSummary {
        assistant_text: String::new(),
        turns: 0,
        input_tokens: 0,
        output_tokens: 0,
    }
}

/// Build a terminal (`Cleared`) `ResumeToken` from a live `GoalState`.
///
/// Every goal-clear path in `drive_goal_loop` persists the same wire shape —
/// a `ResumeToken` whose embedded `GoalSnapshot` is tagged `Cleared { by }`.
/// Centralizing it keeps the `started_at_unix` conversion and field mapping in
/// one place. Pure and synchronous: it holds no lock and performs no I/O, so
/// callers may invoke it while the goal-slot guard is held without changing
/// locking behavior.
fn cleared_resume_token(
    session_id: &str,
    last_turn: u32,
    state: &origin_goal::GoalState,
    by: origin_goal::ClearReasonWire,
) -> origin_resume_token::ResumeToken {
    origin_resume_token::ResumeToken {
        session_id: session_id.to_string(),
        last_turn,
        cas_handle_root: [0u8; 32],
        pending_tool_calls: Vec::new(),
        plan_seq: 0,
        goal: Some(origin_goal::GoalSnapshot {
            condition: state.condition.clone(),
            iter: state.iter,
            max_iter: state.max_iter,
            tokens_spent: state.tokens_spent,
            token_budget: state.token_budget,
            started_at_unix: state
                .started_at
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            status: origin_goal::GoalStatusWire::Cleared { by },
            last_status_tag: state.last_status_tag.clone().map(Into::into),
        }),
        detached_at_unix: None,
        memory_estimate_bytes: None,
    }
}

/// Outcome of the top-of-iteration cap check in [`drive_goal_loop`].
enum GoalCapOutcome {
    /// No cap fired (or no goal active); proceed with the normal turn.
    Continue,
    /// The cap fired on the FIRST iteration. The goal is now cleared; the
    /// caller must still run the user's prompt once (with no goal block) and
    /// return that summary (Bug #7).
    ClearedFirstIter,
    /// The cap fired mid-loop. The caller returns its accumulated summary.
    ClearedMidLoop,
}

/// Top-of-iteration cap check: if a goal exists and is already over budget /
/// max-iter, skip the provider call entirely, clear the slot, persist a
/// terminal checkpoint, and emit `GoalCleared`.
///
/// Reads `session` only (id + message count) and never mutates it. The goal
/// slot lock is taken, the snapshot/token is built while it is held, the slot
/// is set to `None`, and the lock is released before the best-effort store
/// write and event send — matching the original inline scope.
async fn goal_cap_clear(
    active_goal: &tokio::sync::Mutex<Option<origin_goal::GoalState>>,
    session: &Session,
    session_store: &SessionStore,
    event_tx: &tokio::sync::mpsc::Sender<StreamEvent>,
    is_first_iter: bool,
) -> GoalCapOutcome {
    // Build the terminal token + GoalCleared payload under the slot lock, then
    // release the guard before the store write / event send. The token is
    // constructed from `g` before `*slot = None` (so `g`'s borrow ends first),
    // mirroring the original flat ordering; `cleared_resume_token` is pure and
    // performs no await, so building it under the lock is observationally
    // identical to building it after the guard drops. The two `let ... else`
    // arms return early (releasing the guard) when no goal is active or the cap
    // has not fired — the common "keep going" path.
    let (token, cleared_ev) = {
        let mut slot = active_goal.lock().await;
        let Some(g) = slot.as_mut() else {
            return GoalCapOutcome::Continue;
        };
        let Some(reason) = g.cap_check() else {
            return GoalCapOutcome::Continue;
        };
        let iter = g.iter;
        let tokens_spent = g.tokens_spent;
        let wire: origin_goal::ClearReasonWire = reason.into();
        let last_turn = u32::try_from(session.messages.len().saturating_sub(1)).unwrap_or(u32::MAX);
        let token = cleared_resume_token(&session.id, last_turn, g, wire.clone());
        *slot = None;
        // Intentional early drop: release the slot right after clearing it, so
        // the lock is not held while the return tuple is built or during the
        // best-effort store write / event send below (clippy
        // `significant_drop_tightening`). Mirrors the original `*slot = None;
        // drop(slot);` ordering.
        drop(slot);
        (
            token,
            StreamEvent::GoalCleared {
                reason: wire,
                iter,
                tokens_spent,
            },
        )
    };
    if let Err(e) = session_store.save_resume_token(&token) {
        warn!(error = %e, "goal checkpoint: cap-clear save failed");
    }
    let _ = event_tx.send(cleared_ev).await;
    // Bug #7: if this is the FIRST iteration (the user just sent a Prompt and
    // we haven't called run_loop yet), the caller runs their prompt once with
    // the goal now `None` so the system prompt won't include the goal block.
    // Otherwise we'd silently drop the user's input.
    if is_first_iter {
        GoalCapOutcome::ClearedFirstIter
    } else {
        GoalCapOutcome::ClearedMidLoop
    }
}

/// Between-iteration peek for a pending client message (the `Iterate` arm of
/// [`drive_goal_loop`]). Returns `true` when a frame was waiting — in which
/// case the goal has been cleared, a terminal checkpoint persisted, the
/// `GoalCleared` event emitted, and (for non-`Interrupt` messages) the parsed
/// `ClientMessage` pushed into `pending_message` — and the caller should
/// return. Returns `false` when nothing was waiting and the loop should
/// continue. Reads `session` only (id + message count).
async fn handle_iterate_pending(
    conn: &SharedConnection,
    session: &Session,
    active_goal: &tokio::sync::Mutex<Option<origin_goal::GoalState>>,
    session_store: &SessionStore,
    pending_message: &tokio::sync::Mutex<Option<ClientMessage>>,
    event_tx: &tokio::sync::mpsc::Sender<StreamEvent>,
) -> bool {
    // Peek for a pending user message between iterations. If one is waiting,
    // parse it and decide:
    //   * `Interrupt`         → clear the goal, drop the frame (Interrupt is
    //                           itself a no-op after the clear).
    //   * any other variant   → clear the goal AND push the parsed
    //                           `ClientMessage` into `pending_message` so the
    //                           outer message loop dispatches it on its next
    //                           tick (replaces the previous "drop the frame"
    //                           behaviour that silently lost the user's
    //                           follow-up).
    //   * decode failure      → clear the goal, drop the body (a malformed
    //                           frame is the same as an Interrupt for our
    //                           purposes; the outer loop would reject it on
    //                           the next read).
    let peek = {
        let mut g = conn.lock().await;
        tokio::time::timeout(std::time::Duration::ZERO, g.read_frame_body()).await
    };
    let Ok(Ok(pending_body)) = peek else {
        return false;
    };
    // Mirror the outer loop's decode path: ClientMessage envelope first,
    // legacy raw PromptRequest fallback.
    let parsed: Option<ClientMessage> = serde_json::from_slice::<ClientMessage>(&pending_body)
        .ok()
        .or_else(|| {
            #[allow(deprecated)]
            from_legacy_prompt_request(&pending_body).ok()
        });
    let is_interrupt = matches!(parsed, Some(ClientMessage::Interrupt));
    // Bug #12: if the peeked frame couldn't be decoded as any known message,
    // write an ErrorFrame to the client so the user sees that their malformed
    // prompt was dropped (mirrors the outer-loop decode path at
    // main.rs:744-750). Without this the daemon would silently swallow the
    // body and emit only GoalCleared.
    if parsed.is_none() {
        let _ = conn
            .lock()
            .await
            .write_frame(
                FrameKind::ErrorFrame,
                b"bad request: malformed mid-goal frame; dropped",
            )
            .await;
    }
    // Clear the active goal before yielding control. The outer loop sees a
    // stable `None` slot when it picks up the pushed-back message. Build a
    // terminal checkpoint from the prior goal first so a crash between here
    // and the next message write does not resurrect a now-stale Active
    // snapshot.
    let mut slot = active_goal.lock().await;
    let prior = slot.take();
    drop(slot);
    let cleared_ev = prior.as_ref().map(|p| StreamEvent::GoalCleared {
        reason: origin_goal::ClearReasonWire::UserSlash,
        iter: p.iter,
        tokens_spent: p.tokens_spent,
    });
    if let Some(p) = prior {
        let last_turn = u32::try_from(session.messages.len().saturating_sub(1)).unwrap_or(u32::MAX);
        let token = cleared_resume_token(
            &session.id,
            last_turn,
            &p,
            origin_goal::ClearReasonWire::UserSlash,
        );
        if let Err(e) = session_store.save_resume_token(&token) {
            warn!(error = %e, "goal checkpoint: user-slash save failed");
        }
    }
    if let Some(ev) = cleared_ev {
        let _ = event_tx.send(ev).await;
    }
    // Push back only when the user's intent was something OTHER than a plain
    // interrupt (a follow-up Prompt, an admin call, etc). Interrupt itself is
    // consumed here — its job was to fire the GoalCleared we just emitted.
    if !is_interrupt {
        if let Some(msg) = parsed {
            *pending_message.lock().await = Some(msg);
        }
    }
    true
}

/// Apply a `DriverDecision::Cleared`: persist a terminal-status checkpoint and
/// emit the `GoalCleared` event. The caller returns its accumulated summary
/// afterwards. Reads `session` only (id + message count).
async fn handle_goal_cleared(
    active_goal: &tokio::sync::Mutex<Option<origin_goal::GoalState>>,
    session: &Session,
    session_store: &SessionStore,
    event_tx: &tokio::sync::mpsc::Sender<StreamEvent>,
    reason: origin_goal::ClearReasonWire,
    iter: u32,
    tokens_spent: u64,
) {
    // Build a terminal-status snapshot ourselves so the checkpoint reflects
    // the final wire-shape, then clear the slot. Doing this BEFORE the
    // `take()` would require a reverse `From<ClearReasonWire>` for
    // `ClearReason` (we currently only have the forward); building the
    // snapshot directly is simpler and keeps the inverse mapping in one place.
    let terminal_token = {
        let mut slot = active_goal.lock().await;
        slot.take().map(|g| {
            let last_turn = u32::try_from(session.messages.len().saturating_sub(1)).unwrap_or(u32::MAX);
            cleared_resume_token(&session.id, last_turn, &g, reason.clone())
        })
    };
    if let Some(token) = terminal_token {
        if let Err(e) = session_store.save_resume_token(&token) {
            warn!(error = %e, "goal checkpoint: terminal save failed");
        }
    }
    // `handle_resume_request` only re-installs Active / Verifying snapshots — a
    // terminal Cleared snapshot is correctly ignored on the next resume.
    let _ = event_tx
        .send(StreamEvent::GoalCleared {
            reason,
            iter,
            tokens_spent,
        })
        .await;
}

/// Record the just-completed iteration against the goal, run the verifier, and
/// apply the resulting mutations — returning the [`DriverDecision`] to act on.
///
/// Returns `None` when the goal slot was cleared by another path (e.g.
/// `/-goal`) while this turn ran; the caller treats that as "done, return the
/// current summary". `input_tokens` / `output_tokens` come from the turn's
/// `LoopSummary`.
///
/// Bug #6: the goal slot lock is held only across the snapshot and across the
/// mutation apply — never across the verifier's network round-trip in
/// `drive_decision`. Each guard is dropped explicitly at its last use so the
/// lock is not held over the intervening await (clippy
/// `significant_drop_tightening`).
async fn run_verifier_dispatch(
    session: &Session,
    active_goal: &tokio::sync::Mutex<Option<origin_goal::GoalState>>,
    verifier: &dyn origin_goal::verifier::Verifier,
    event_tx: &tokio::sync::mpsc::Sender<StreamEvent>,
    input_tokens: u64,
    output_tokens: u64,
) -> Option<origin_daemon::goal_driver::DriverDecision> {
    let last_text = last_assistant_text(session);
    let tag = origin_goal::parse_tag(&last_text);
    let inputs = {
        let mut slot = active_goal.lock().await;
        // `None` here means the goal was cleared by another path while we ran;
        // `?` short-circuits the whole fn so the caller returns its summary.
        let g = slot.as_mut()?;
        g.record_iteration(input_tokens, output_tokens, tag);
        // Emit `GoalVerifying` BEFORE calling the verifier so the CLI's status
        // line flips before the Haiku call latency lands.
        if matches!(g.last_status_tag, Some(origin_goal::TagOutcome::Met)) {
            let _ = event_tx.send(StreamEvent::GoalVerifying).await;
        }
        let snapshot = origin_daemon::goal_driver::DriverInputs::snapshot(g);
        // Intentional early drop: release the slot immediately after the last
        // read of `g` so the lock is not held while `drive_decision` awaits the
        // verifier. Releases at the same point the block's closing brace would.
        drop(slot);
        snapshot
    };
    // Lock is DROPPED here — the verifier's network round-trip in
    // `drive_decision` runs without serializing the slot.
    let outcome = origin_daemon::goal_driver::drive_decision(inputs, &last_text, verifier).await;
    let mut slot = active_goal.lock().await;
    let g = slot.as_mut()?;
    let decision = origin_daemon::goal_driver::apply_outcome(g, outcome);
    // Intentional early drop: release the slot right after `apply_outcome`
    // mutates `g` (clippy `significant_drop_tightening`).
    drop(slot);
    Some(decision)
}

/// Goal-driver loop wrapping `run_loop`. When no goal is active this is a
/// single passthrough call; when a goal IS active the driver re-enters
/// `run_loop` with synthesized continuation prompts, emitting `GoalIteration`
/// events between turns, until the driver decides to clear (verified,
/// cap-hit, etc.). Between iterations we peek at the connection's incoming-
/// message channel via a zero-duration `timeout` poll — if any other
/// `ClientMessage` is waiting, we break out so the outer message loop
/// handles it (the spec's "user interrupt mid-iteration" case).
#[allow(clippy::too_many_arguments)]
async fn drive_goal_loop(
    conn: &SharedConnection,
    session: &mut Session,
    initial_user_text: String,
    provider: &dyn Provider,
    opts: &LoopOptions,
    active_goal: Arc<tokio::sync::Mutex<Option<origin_goal::GoalState>>>,
    pending_message: Arc<tokio::sync::Mutex<Option<ClientMessage>>>,
    session_store: Arc<SessionStore>,
    verifier: &dyn origin_goal::verifier::Verifier,
    event_tx: tokio::sync::mpsc::Sender<StreamEvent>,
) -> Result<origin_daemon::agent::LoopSummary, origin_daemon::agent::LoopError> {
    use origin_daemon::goal_checkpoint::make_goal_checkpoint_token;
    use origin_daemon::goal_driver::DriverDecision;
    // Local closure: build a ResumeToken from the current goal state and
    // persist it. Best-effort — a write failure should not interrupt the
    // iteration. `session.messages.len()` is the highest persisted-or-
    // about-to-be-persisted turn index; we subtract one to refer to the
    // last completed turn, saturating at 0 so an empty session reports
    // `last_turn: 0` rather than panicking on underflow.
    let checkpoint = |sess: &Session| {
        let last_turn = u32::try_from(sess.messages.len().saturating_sub(1)).unwrap_or(u32::MAX);
        let store = Arc::clone(&session_store);
        let goal_slot = Arc::clone(&active_goal);
        let session_id = sess.id.clone();
        async move {
            let token = {
                let guard = goal_slot.lock().await;
                make_goal_checkpoint_token(&session_id, last_turn, &guard)
            };
            if let Err(e) = store.save_resume_token(&token) {
                warn!(error = %e, "goal checkpoint: save failed; iteration continues");
            }
        }
    };
    let mut next_text = initial_user_text;
    let mut last_summary: Option<origin_daemon::agent::LoopSummary> = None;
    // Bug #7: on the first iteration, if the cap fires we still need to
    // give the user's original prompt a normal turn (without the goal
    // block in the system prompt, since the goal is now cleared). We
    // track whether we're on the first iteration so the cap-clear branch
    // can fall through into one more `run_loop` call instead of bailing
    // with a synthetic empty summary.
    let mut is_first_iter = true;
    loop {
        // Top-of-iteration cap check: if a goal is already over budget /
        // max-iter, clear it before the provider call (see `goal_cap_clear`).
        match goal_cap_clear(&active_goal, session, &session_store, &event_tx, is_first_iter).await {
            GoalCapOutcome::Continue => {}
            // Mid-loop cap: return whatever summary we have.
            GoalCapOutcome::ClearedMidLoop => {
                return Ok(last_summary.unwrap_or_else(empty_loop_summary));
            }
            // Bug #7: cap fired on the first iteration — run the user's prompt
            // once with no active goal, then return. This guarantees the
            // user's prompt is never silently dropped.
            GoalCapOutcome::ClearedFirstIter => {
                let summary =
                    origin_daemon::agent::run_loop(session, &next_text, provider, &AlwaysAllow, opts).await?;
                return Ok(summary);
            }
        }

        let summary =
            origin_daemon::agent::run_loop(session, &next_text, provider, &AlwaysAllow, opts).await?;
        is_first_iter = false;

        // If no goal is active, we're done after one turn.
        let goal_active = active_goal.lock().await.is_some();
        if !goal_active {
            return Ok(summary);
        }

        // Record this iteration, run the verifier off-lock, and apply the
        // resulting mutations (Bug #6). `None` means the goal was cleared by
        // another path mid-turn — return what we have.
        let Some(decision) = run_verifier_dispatch(
            session,
            &active_goal,
            verifier,
            &event_tx,
            summary.input_tokens,
            summary.output_tokens,
        )
        .await
        else {
            return Ok(summary);
        };
        last_summary = Some(summary);

        // Persist a fresh goal-aware ResumeToken AFTER record_iteration so
        // a crash between iterations restarts mid-goal at the correct
        // tokens_spent / iter counters. Best-effort — see closure body.
        checkpoint(session).await;

        match decision {
            DriverDecision::Iterate {
                synthesized_prompt,
                iter_event,
            } => {
                let _ = event_tx.send(iter_event).await;
                next_text = synthesized_prompt;
                // Peek for a pending user message between iterations; if one is
                // waiting, the goal is cleared and we return (see
                // `handle_iterate_pending`). Otherwise fall through and loop.
                if handle_iterate_pending(
                    conn,
                    session,
                    &active_goal,
                    &session_store,
                    &pending_message,
                    &event_tx,
                )
                .await
                {
                    return Ok(last_summary.unwrap_or_else(empty_loop_summary));
                }
            }
            DriverDecision::Cleared {
                reason,
                iter,
                tokens_spent,
            } => {
                handle_goal_cleared(
                    &active_goal,
                    session,
                    &session_store,
                    &event_tx,
                    reason,
                    iter,
                    tokens_spent,
                )
                .await;
                return Ok(last_summary.unwrap_or_else(empty_loop_summary));
            }
        }
    }
}

/// If a workflow is in progress on this connection, advance past the
/// step that just completed. Deactivates the current step's skill,
/// activates the next resolvable step's skill, and emits the
/// corresponding `WorkflowStepActive` or `WorkflowComplete` event.
/// No-op when no workflow is active.
///
/// Called unconditionally after every `Prompt` turn end — both success
/// and provider-error paths advance the workflow, since reaching the
/// end of a turn IS the gate. Users re-activate the workflow if they
/// need to restart from step 0.
async fn advance_workflow(
    conn: &SharedConnection,
    active_workflow: Arc<tokio::sync::Mutex<Option<origin_daemon::workflow_progress::WorkflowProgress>>>,
    active_skills: Arc<tokio::sync::Mutex<SkillRegistry>>,
    skill_catalog: Arc<origin_daemon::skill_catalog::SkillCatalog>,
) {
    use origin_daemon::workflow_progress::AdvanceOutcome;
    let mut wf_guard = active_workflow.lock().await;
    let Some(progress) = wf_guard.as_mut() else {
        return;
    };
    let outcome = progress.advance(skill_catalog.as_ref());
    let ev = match outcome {
        AdvanceOutcome::Stepped {
            previous_skill,
            front,
            skipped,
        } => {
            let name = progress.name.clone();
            let step_index = u32::try_from(progress.current_step_index).unwrap_or(u32::MAX);
            let total_steps = u32::try_from(progress.total_steps).unwrap_or(u32::MAX);
            let skill = progress.current_skill.clone();
            let mut skills = active_skills.lock().await;
            skills.deactivate(&previous_skill);
            skills.activate(front);
            drop(skills);
            StreamEvent::WorkflowStepActive {
                name,
                step_index,
                total_steps,
                skill,
                skipped,
            }
        }
        AdvanceOutcome::Complete {
            previous_skill,
            skipped,
        } => {
            let name = progress.name.clone();
            active_skills.lock().await.deactivate(&previous_skill);
            *wf_guard = None;
            StreamEvent::WorkflowComplete { name, skipped }
        }
    };
    // Release the workflow lock before the IPC write — write_frame can
    // suspend on a slow consumer and we don't want to hold the per-conn
    // workflow mutex for that span.
    drop(wf_guard);
    let body = serde_json::to_vec(&ev).unwrap_or_default();
    let _ = conn.lock().await.write_frame(FrameKind::Event, &body).await;
}

/// If a workflow is in progress and the prompt that just ran failed,
/// emit a `WorkflowStepHeld` event. The workflow stays paused at the
/// same step — its skill remains on the active stack — and the next
/// successful `Prompt` will trigger `advance_workflow`. No-op when no
/// workflow is active.
async fn hold_workflow(
    conn: &SharedConnection,
    active_workflow: Arc<tokio::sync::Mutex<Option<origin_daemon::workflow_progress::WorkflowProgress>>>,
    message: &str,
) {
    let snapshot = {
        let wf_guard = active_workflow.lock().await;
        wf_guard.as_ref().map(|progress| {
            (
                progress.name.clone(),
                u32::try_from(progress.current_step_index).unwrap_or(u32::MAX),
                u32::try_from(progress.total_steps).unwrap_or(u32::MAX),
                progress.current_skill.clone(),
            )
        })
    };
    let Some((name, step_index, total_steps, skill)) = snapshot else {
        return;
    };
    let ev = StreamEvent::WorkflowStepHeld {
        name,
        step_index,
        total_steps,
        skill,
        message: message.to_string(),
    };
    let body = serde_json::to_vec(&ev).unwrap_or_default();
    let _ = conn.lock().await.write_frame(FrameKind::Event, &body).await;
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

    // Keep the process-wide factory's account in sync so a subsequent
    // CROSS-provider router rebuild (`provider_factory::build_provider_for`)
    // resolves credentials for the freshly-switched account rather than the
    // startup default. No-op unless `set_global` was called at startup (i.e.
    // cross-provider routing is active), so the default path is unchanged.
    origin_daemon::provider_factory::update_global_account(account);

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
        ClientMessage::RewindSession {
            session_id,
            keep_turns,
        } => match session_store.truncate_after(&session_id, keep_turns) {
            Ok(_removed) => StreamEvent::AdminOk,
            Err(e) => StreamEvent::AdminError {
                message: e.to_string(),
            },
        },
        ClientMessage::ResumeSession { session_id } => resume_session_event(session_store, &session_id),
        ClientMessage::ResumeForeign { source, path } => {
            resume_foreign_event(session_store, &source, &path)
        }
        ClientMessage::ExportSession { session_id, format } => {
            export_session_event(session_store, &session_id, &format)
        }
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
        | ClientMessage::Interrupt
        | ClientMessage::ClearAll
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
            |((provider, model), (tokens_in, tokens_out))| {
                // Cost from output + fresh-input tokens (the metrics registry
                // does not split cache tiers per model, so this is a floor).
                let cost_usd = origin_cost::price_for(&model).map_or(0.0, |p| {
                    let u = origin_cost::TokenUsage::new(tokens_in, tokens_out, 0, 0);
                    origin_cost::cost_of(&p, &u).total()
                });
                origin_daemon::protocol::UsageRow {
                    provider,
                    model,
                    tokens_in,
                    tokens_out,
                    cost_usd,
                }
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
/// Build an [`origin_export::ExportSession`] from the persisted log and render
/// it as Markdown (`format == "json"` selects JSON instead). Replies with
/// [`StreamEvent::SessionExport`] or [`StreamEvent::AdminError`].
fn export_session_event(session_store: &SessionStore, session_id: &str, format: &str) -> StreamEvent {
    use origin_core::types::Block;
    let messages = match session_store.load_messages(session_id) {
        Ok(m) => m,
        Err(e) => {
            return StreamEvent::AdminError {
                message: e.to_string(),
            }
        }
    };
    let summary = session_store
        .list_summaries()
        .unwrap_or_default()
        .into_iter()
        .find(|s| s.id == session_id);
    if messages.is_empty() && summary.is_none() {
        return StreamEvent::AdminError {
            message: format!("session not found: {session_id}"),
        };
    }
    let (title, model, created_at) = summary.map_or((None, String::new(), 0), |s| {
        let ms = u64::try_from(s.created_at).unwrap_or(0);
        (s.title, s.model, ms)
    });

    let turns = messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
                Role::System => "system",
            }
            .to_string();
            let mut text = String::new();
            let mut tools = Vec::new();
            for b in &m.blocks {
                match b {
                    Block::Text { text: t, .. } => {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(t);
                    }
                    Block::ToolUse { name, .. } => tools.push(name.clone()),
                    Block::Thinking { .. } | Block::ToolResult { .. } => {}
                }
            }
            origin_export::ExportTurn { role, text, tools }
        })
        .collect();

    let session = origin_export::ExportSession {
        id: session_id.to_string(),
        title,
        provider: String::new(),
        model,
        created_at_unix_ms: created_at,
        turns,
    };

    let content = if format == "json" {
        match origin_export::to_json(&session) {
            Ok(s) => s,
            Err(e) => {
                return StreamEvent::AdminError {
                    message: e.to_string(),
                }
            }
        }
    } else {
        origin_export::to_markdown(&session)
    };
    StreamEvent::SessionExport { content }
}

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

/// Build the [`StreamEvent`] reply for a [`ClientMessage::ResumeForeign`].
///
/// Cross-harness live resume: map the `source` tag to a
/// [`SourceKind`](origin_migrate::reconstruct::SourceKind), reconstruct the
/// foreign transcript at `path` into origin's native message model, then CREATE
/// a fresh origin session seeded with those messages via the same
/// `persist_session` + `persist_message` path the live agent loop uses (the
/// [`persist`] helper). The new session adopts the reconstructed
/// `suggested_model`. Replies with [`StreamEvent::ForeignResumed`] carrying the
/// new id + persisted count + model, or [`StreamEvent::AdminError`] on an
/// unknown source tag, a missing path, or a parse/I-O failure — never panics.
fn resume_foreign_event(session_store: &SessionStore, source: &str, path: &str) -> StreamEvent {
    use origin_migrate::reconstruct::{reconstruct_from_path, SourceKind};

    let Some(kind) = SourceKind::from_tag(source) else {
        return StreamEvent::AdminError {
            message: format!("unknown foreign source: {source:?} (expected claude-code | jcode | opencode)"),
        };
    };
    // Reject empty paths before touching the filesystem; `reconstruct_from_path`
    // itself validates existence and surfaces parse/IO failures as SourceError.
    if path.trim().is_empty() {
        return StreamEvent::AdminError {
            message: "empty path for foreign resume".to_string(),
        };
    }
    let resumed = match reconstruct_from_path(kind, std::path::Path::new(path), None) {
        Ok(r) => r,
        Err(e) => {
            return StreamEvent::AdminError {
                message: format!("reconstruct {source} session at {path}: {e}"),
            };
        }
    };

    // Seed a brand-new origin session with the reconstructed transcript and
    // persist it through the SAME create+append path the agent loop uses.
    let mut session = Session::new(String::new(), resumed.suggested_model.clone());
    session.messages = resumed.messages;
    persist(session_store, &session);

    // Saturate at u32::MAX — a transcript with >4 G messages is not feasible.
    #[allow(clippy::cast_possible_truncation)]
    let messages_loaded = u32::try_from(session.messages.len()).unwrap_or(u32::MAX);
    info!(
        source,
        path,
        session = %session.id,
        messages_loaded,
        suggested_model = %resumed.suggested_model,
        "resume-foreign: hydrated new session"
    );
    StreamEvent::ForeignResumed {
        session_id: session.id,
        messages_loaded,
        suggested_model: resumed.suggested_model,
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

// ── install-ra subcommand ─────────────────────────────────────────────────────

/// Download a rust-analyzer binary into `$ORIGIN_CACHE/bin` for the
/// `Diagnostics` tool to use.
///
/// The downloaded file is the raw archive from the GitHub release
/// (`*.gz` on Linux/macOS, `*.zip` on Windows). Automatic extraction is
/// intentionally omitted for v2 simplicity — the user runs `gunzip` /
/// `Expand-Archive` manually after download. A future v2.1 pass can add
/// automatic decompression.
fn install_ra() -> Result<()> {
    let cache_dir = resolve_install_cache_dir()?;
    let bin_dir = cache_dir.join("bin");
    std::fs::create_dir_all(&bin_dir)
        .map_err(|e| anyhow::anyhow!("create bin dir {}: {e}", bin_dir.display()))?;

    let (url, file_name) = ra_release_url_for_platform();
    if url.is_empty() {
        return Err(anyhow::anyhow!("unsupported platform for install-ra"));
    }
    let target = bin_dir.join(file_name);
    eprintln!("origin: downloading rust-analyzer from {url}");
    let bytes = reqwest::blocking::get(url.as_str())
        .map_err(|e| anyhow::anyhow!("download failed: {e}"))?
        .bytes()
        .map_err(|e| anyhow::anyhow!("read response: {e}"))?;
    std::fs::write(&target, &bytes).map_err(|e| anyhow::anyhow!("write {}: {e}", target.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&target)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&target, perms)
            .map_err(|e| anyhow::anyhow!("chmod {}: {e}", target.display()))?;
    }
    eprintln!(
        "origin: installed: {} — run gunzip/unzip to extract, then retry Diagnostics",
        target.display()
    );
    Ok(())
}

fn resolve_install_cache_dir() -> Result<std::path::PathBuf> {
    if let Ok(c) = std::env::var("ORIGIN_CACHE") {
        return Ok(c.into());
    }
    #[cfg(windows)]
    if let Ok(c) = std::env::var("LOCALAPPDATA") {
        return Ok(std::path::PathBuf::from(c).join("origin"));
    }
    #[cfg(not(windows))]
    if let Ok(c) = std::env::var("XDG_CACHE_HOME") {
        return Ok(std::path::PathBuf::from(c).join("origin"));
    }
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home directory found"))?
        .join(".cache")
        .join("origin"))
}

/// Return `(download_url, archive_file_name)` for the current platform.
/// Returns `("", "rust-analyzer")` on unsupported platforms.
fn ra_release_url_for_platform() -> (String, &'static str) {
    let base = "https://github.com/rust-lang/rust-analyzer/releases/latest/download";
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return (
        format!("{base}/rust-analyzer-x86_64-unknown-linux-gnu.gz"),
        "rust-analyzer.gz",
    );
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return (
        format!("{base}/rust-analyzer-aarch64-unknown-linux-gnu.gz"),
        "rust-analyzer.gz",
    );
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return (
        format!("{base}/rust-analyzer-aarch64-apple-darwin.gz"),
        "rust-analyzer.gz",
    );
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return (
        format!("{base}/rust-analyzer-x86_64-apple-darwin.gz"),
        "rust-analyzer.gz",
    );
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return (
        format!("{base}/rust-analyzer-x86_64-pc-windows-msvc.zip"),
        "rust-analyzer.zip",
    );
    #[allow(unreachable_code)]
    (String::new(), "rust-analyzer")
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

#[cfg(test)]
mod tests {
    use super::{resume_foreign_event, SessionStore, StreamEvent};

    /// End-to-end (sans IPC): reconstruct a Claude Code transcript at a path and
    /// persist it into a fresh origin session. The reply's `messages_loaded`
    /// must equal the number of rows actually written to the store, and the new
    /// session must adopt the reconstructed model.
    #[test]
    #[allow(clippy::panic)] // test asserts the StreamEvent variant via a panicking else-arm
    fn resume_foreign_persists_and_counts() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Claude Code harness layout: <root>/projects/<proj>/<id>.jsonl
        let root = dir.path().join("cc");
        let proj = root.join("projects").join("demo");
        std::fs::create_dir_all(&proj).expect("mkdir");
        let jsonl = "{\"type\":\"human\",\"content\":\"fix the build\"}\n\
                     {\"type\":\"assistant\",\"content\":\"patching Cargo.toml\"}\n\
                     {\"type\":\"human\",\"content\":\"now run tests\"}\n";
        std::fs::write(proj.join("abc.jsonl"), jsonl).expect("write transcript");

        let store = SessionStore::open(dir.path().join("sessions.db")).expect("open store");
        let ev = resume_foreign_event(&store, "claude-code", &root.display().to_string());

        match ev {
            StreamEvent::ForeignResumed {
                session_id,
                messages_loaded,
                suggested_model,
            } => {
                assert_eq!(messages_loaded, 3, "three transcript turns");
                assert_eq!(suggested_model, "claude-sonnet-4-6");

                // The persisted row count must match what the reply reported.
                let persisted = store.load_messages(&session_id).expect("load_messages");
                assert_eq!(u32::try_from(persisted.len()).expect("fits u32"), messages_loaded);
                // And the session is listed (resumable) with the reconstructed model.
                let summary = store
                    .list_summaries()
                    .expect("list")
                    .into_iter()
                    .find(|s| s.id == session_id)
                    .expect("new session present");
                assert_eq!(summary.model, "claude-sonnet-4-6");
                assert_eq!(summary.message_count, 3);
            }
            other => panic!("expected ForeignResumed, got {other:?}"),
        }
    }

    /// An unknown source tag must fail with `AdminError` and never persist.
    #[test]
    fn resume_foreign_unknown_source_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::open(dir.path().join("sessions.db")).expect("open store");
        let ev = resume_foreign_event(&store, "not-a-harness", "/whatever");
        assert!(matches!(ev, StreamEvent::AdminError { .. }), "got {ev:?}");
        assert!(store.list_summaries().expect("list").is_empty());
    }

    /// A missing path must fail with `AdminError` (validated before any read).
    #[test]
    fn resume_foreign_missing_path_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::open(dir.path().join("sessions.db")).expect("open store");
        let missing = dir.path().join("does-not-exist");
        let ev = resume_foreign_event(&store, "claude-code", &missing.display().to_string());
        assert!(matches!(ev, StreamEvent::AdminError { .. }), "got {ev:?}");
        assert!(store.list_summaries().expect("list").is_empty());
    }
}
