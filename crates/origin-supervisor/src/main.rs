// SPDX-License-Identifier: Apache-2.0
//! `origin-supervisor` — restarts `origin-daemon` on crash; coordinates session
//! resume tokens across the restart.

use clap::Parser;
use origin_supervisor::ipc_resume;
use origin_supervisor::relaunch::{
    decide_relaunch, default_relaunch_manifest_path, load_manifest, perform_swap, watch_outcome,
    RelaunchDecision, RelaunchManifest, WatchOutcome, SELFDEV_RELAUNCH_EXIT_CODE,
};
use origin_supervisor::resume_token::ResumeToken;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

/// Number of connect attempts the resume replay makes while the freshly
/// (re)spawned daemon is still binding its IPC endpoint. Each failed attempt
/// sleeps [`RESUME_CONNECT_BACKOFF`] before retrying; the product bounds how
/// long the detached replay thread lingers when no daemon ever comes up.
const RESUME_CONNECT_ATTEMPTS: u32 = 40;
/// Delay between resume-replay connect attempts (≈ `40 × 250ms = 10s` total).
const RESUME_CONNECT_BACKOFF: Duration = Duration::from_millis(250);

/// Crash streak (non-sentinel exits) within [`ROLLBACK_WINDOW`] of a swap that
/// triggers an automatic rollback to the previous binary.
const ROLLBACK_THRESHOLD: u32 = 2;
/// Observation window after a swap during which a crash streak counts toward a
/// rollback. A swapped binary that survives this window is deemed healthy.
const ROLLBACK_WINDOW: Duration = Duration::from_secs(5);

/// State tracked while watching a freshly swapped-in binary for crash-looping.
struct SwapWatch {
    /// When the swap was performed (start of the observation window).
    swapped_at: Instant,
    /// Non-sentinel exits observed since the swap.
    crashes_since_swap: u32,
    /// The backup of the previous binary, restored on rollback.
    backup_path: PathBuf,
}

/// The backup path for `daemon_path`: a sibling `<name>.bak` so the rename in
/// [`perform_swap`] stays within one directory.
fn backup_path_for(daemon_path: &Path) -> PathBuf {
    let mut name = daemon_path.file_name().map_or_else(
        || std::ffi::OsString::from("origin-daemon"),
        std::ffi::OsStr::to_os_string,
    );
    name.push(".bak");
    daemon_path.with_file_name(name)
}

/// The scratch backup path used when restoring the backup OVER `daemon_path`
/// during a rollback (a distinct sibling so it never clobbers the live backup).
fn rollback_scratch_path(daemon_path: &Path) -> PathBuf {
    let mut name = daemon_path.file_name().map_or_else(
        || std::ffi::OsString::from("origin-daemon"),
        std::ffi::OsStr::to_os_string,
    );
    name.push(".rollback-scratch");
    daemon_path.with_file_name(name)
}

/// A freshly built binary is "valid" to swap in when its path exists and is a
/// non-empty file. Guards against swapping in a truncated/half-written artifact.
fn new_binary_valid(manifest: &RelaunchManifest) -> bool {
    std::fs::metadata(&manifest.new_binary_path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// Resolve the `resume/` directory the daemon persists its [`ResumeToken`]s
/// into, deriving it exactly as the daemon does so the supervisor reads the
/// same files the daemon writes.
///
/// The daemon's session DB path is `ORIGIN_DB` (when set by the CLI/tests) or
/// the per-instance default `InstanceId::for_cwd().db_path()`. The supervisor
/// inherits the CLI's cwd and env, so `for_cwd()` yields the same instance id.
/// `SessionStore` stores resume tokens under `<dir-of-db>/resume`
/// (`SessionStore::resume_dir`), so we mirror that derivation here.
fn resolve_resume_dir() -> PathBuf {
    let db_path = std::env::var("ORIGIN_DB")
        .unwrap_or_else(|_| origin_ipc::instance::InstanceId::for_cwd().db_path());
    let db = PathBuf::from(db_path);
    let dir = db.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    dir.join("resume")
}

/// Resolve the daemon's IPC endpoint the same way the CLI and daemon do:
/// `ORIGIN_SOCK` when set (the CLI pins it on the supervisor's env), else the
/// per-instance path for the current working directory.
fn resolve_ipc_endpoint() -> String {
    origin_ipc::instance::resolve_ipc_path()
}

/// Best-effort replay of any persisted resume tokens to a freshly (re)spawned
/// daemon, run on a detached thread so it can wait for the daemon's IPC
/// endpoint to come up while the supervisor's main thread blocks on the child.
///
/// DEFAULT-SAFE: when no tokens are present (the common case) this is a no-op —
/// it never even opens a connection, so a daemon with no detached sessions
/// behaves byte-identically to before this wiring existed. Every failure
/// (unreadable dir, tampered token, daemon never reachable) is logged and
/// swallowed: one bad token must never wedge the restart loop.
fn spawn_resume_replay(resume_dir: PathBuf, endpoint: String) {
    std::thread::spawn(move || {
        // Enumerate first; a clean no-op short-circuits before any IPC work.
        let tokens = match ResumeToken::load_all(&resume_dir) {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, dir = %resume_dir.display(),
                    "supervisor: could not load resume tokens; skipping replay");
                return;
            }
        };
        if tokens.is_empty() {
            return;
        }
        let count = tokens.len();
        info!(count, dir = %resume_dir.display(),
            "supervisor: replaying persisted resume tokens to daemon");
        match drive_resume_replay(tokens, &endpoint) {
            Ok(()) => info!(count, "supervisor: resume tokens replayed"),
            Err(e) => warn!(error = %e, "supervisor: resume replay failed (sessions not re-attached)"),
        }
    });
}

/// Connect to the (possibly still-binding) daemon with bounded retry, then
/// replay every token over IPC. Delegates to [`drive_resume_replay_with`] with
/// the production retry budget.
///
/// # Errors
/// Returns the last connect error if the daemon never becomes reachable within
/// [`RESUME_CONNECT_ATTEMPTS`], or any serialization/write error from the replay.
fn drive_resume_replay(tokens: Vec<ResumeToken>, endpoint: &str) -> anyhow::Result<()> {
    drive_resume_replay_with(tokens, endpoint, RESUME_CONNECT_ATTEMPTS, RESUME_CONNECT_BACKOFF)
}

/// Retry-parameterized core of [`drive_resume_replay`] (the `attempts`/`backoff`
/// are arguments so tests can use a tiny budget). Runs the async
/// [`ipc_resume::replay_all`] on a throwaway current-thread runtime so the sync
/// supervisor needs no global one.
///
/// # Errors
/// Returns the last connect error if the daemon never becomes reachable within
/// `attempts`, or any serialization/write error from the replay.
fn drive_resume_replay_with(
    tokens: Vec<ResumeToken>,
    endpoint: &str,
    attempts: u32,
    backoff: Duration,
) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        // The daemon may still be binding its endpoint after spawn; retry the
        // first connect a bounded number of times before giving up.
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..attempts.max(1) {
            match origin_ipc::transport::Connector::connect(endpoint).await {
                Ok(_) => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    last_err = Some(e.into());
                    if attempt + 1 < attempts.max(1) {
                        tokio::time::sleep(backoff).await;
                    }
                }
            }
        }
        if let Some(e) = last_err {
            return Err(anyhow::anyhow!(
                "daemon IPC endpoint {endpoint} not reachable for resume replay: {e}"
            ));
        }
        ipc_resume::replay_all(tokens, endpoint).await
    })
}

#[cfg(unix)]
mod launch_unix;
#[cfg(unix)]
use launch_unix as launch;

#[cfg(windows)]
mod launch_windows;
#[cfg(windows)]
use launch_windows as launch;

#[derive(Debug, Parser)]
#[command(name = "origin-supervisor", version)]
struct Args {
    /// Path to the `origin-daemon` binary. Falls back to `which origin-daemon`.
    #[arg(long)]
    daemon_path: Option<PathBuf>,

    /// Maximum restarts within a sliding 60-second window before giving up.
    #[arg(long, default_value = "12")]
    max_restarts_per_min: u32,

    /// Arguments forwarded to the daemon (separate with `--`).
    #[arg(last = true)]
    forward: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let daemon_path = args
        .daemon_path
        .or_else(|| which::which("origin-daemon").ok())
        .ok_or_else(|| anyhow::anyhow!("origin-daemon not found"))?;
    info!(path = %daemon_path.display(), "supervisor: starting");

    let manifest_path = default_relaunch_manifest_path();
    info!(manifest = %manifest_path.display(), "supervisor: watching for self-dev relaunch manifest");

    run_supervisor_loop(
        &daemon_path,
        &manifest_path,
        args.max_restarts_per_min,
        &args.forward,
    )
}

/// The supervisor's restart loop: spawn the daemon, fold each exit into the
/// crash-storm guard and the self-dev relaunch/rollback machinery, and respawn.
/// Returns `Err` only when the restart-storm cap is exceeded (the one terminal
/// condition); otherwise it loops forever.
fn run_supervisor_loop(
    daemon_path: &Path,
    manifest_path: &Path,
    max_restarts_per_min: u32,
    forward: &[String],
) -> anyhow::Result<()> {
    let mut restarts: Vec<Instant> = Vec::with_capacity(max_restarts_per_min as usize);
    // Active only while observing a freshly swapped-in binary for crash-looping.
    let mut watch: Option<SwapWatch> = None;
    loop {
        let now = Instant::now();
        restarts.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
        let restart_count = u32::try_from(restarts.len()).unwrap_or(u32::MAX);
        if restart_count >= max_restarts_per_min {
            error!(
                "supervisor: {} restarts in the last 60s exceeds the cap; bailing",
                restarts.len()
            );
            return Err(anyhow::anyhow!("restart storm"));
        }

        // Before we (re)spawn and block on the daemon, kick off a detached,
        // best-effort replay of any persisted resume tokens. It waits for the
        // about-to-be-spawned daemon's IPC endpoint to come up, then sends one
        // `ResumeRequest` per token so detached/in-flight sessions are
        // re-attached across the restart — the supervisor's stated purpose.
        // No tokens present → the thread no-ops without opening a connection.
        spawn_resume_replay(resolve_resume_dir(), resolve_ipc_endpoint());

        let exit = launch::run_child(daemon_path, forward)?;
        let code = exit.status.code().unwrap_or(-1);
        let is_sentinel = code == SELFDEV_RELAUNCH_EXIT_CODE;
        warn!(?exit, code, is_sentinel, "supervisor: daemon exited");

        // If we are observing a freshly swapped binary, fold this exit into the
        // rollback policy. A rollback restores the previous binary and restarts
        // it immediately (skipping the manifest consult, which the rollback just
        // cleared). Otherwise consult the relaunch manifest and act on it (swap-in
        // on a sentinel exit with a valid manifest; restart-same otherwise).
        if !fold_swap_watch(daemon_path, manifest_path, &mut watch, is_sentinel) {
            apply_relaunch_decision(daemon_path, manifest_path, code, &mut watch);
        }

        restarts.push(Instant::now());
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Fold the daemon exit into an active post-swap rollback watch.
///
/// Returns `true` when a rollback was performed (the caller should restart the
/// now-restored binary immediately). Returns `false` when there was no active
/// watch, the watch was cleared (healthy), or the watch is still observing — in
/// all of which the caller proceeds to the normal manifest consult.
fn fold_swap_watch(
    daemon_path: &Path,
    manifest_path: &Path,
    watch: &mut Option<SwapWatch>,
    is_sentinel: bool,
) -> bool {
    let Some(w) = watch.as_mut() else { return false };

    // A sentinel exit means the just-swapped binary ran long enough to self-dev
    // again — a healthy outcome handled by the normal manifest consult.
    if is_sentinel {
        info!("supervisor: swapped binary requested a further relaunch; clearing rollback watch");
        *watch = None;
        return false;
    }

    w.crashes_since_swap = w.crashes_since_swap.saturating_add(1);
    let elapsed = w.swapped_at.elapsed();
    match watch_outcome(w.crashes_since_swap, elapsed, ROLLBACK_THRESHOLD, ROLLBACK_WINDOW) {
        WatchOutcome::RollBack => {
            restore_previous_binary(daemon_path, &w.backup_path, w.crashes_since_swap);
            // Drop the manifest so we never re-swap the bad binary, stop watching.
            let _ = std::fs::remove_file(manifest_path);
            *watch = None;
            true
        }
        WatchOutcome::ClearStable => {
            info!("supervisor: swapped binary survived the observation window; clearing rollback watch");
            *watch = None;
            false
        }
        WatchOutcome::KeepWatching => false,
    }
}

/// Restore the previous daemon binary from `backup` over `daemon_path` (atomic
/// via the staging rename in [`perform_swap`]). A restore failure is logged and
/// the supervisor continues on the current binary rather than aborting.
fn restore_previous_binary(daemon_path: &Path, backup: &Path, crashes: u32) {
    let scratch = rollback_scratch_path(daemon_path);
    match perform_swap(backup, daemon_path, &scratch) {
        Ok(()) => warn!(
            crashes,
            backup = %backup.display(),
            "supervisor: swapped binary crash-looped; ROLLED BACK to previous binary"
        ),
        Err(e) => error!(
            error = %e,
            "supervisor: rollback restore FAILED; continuing on current binary"
        ),
    }
}

/// Consult the relaunch manifest for `daemon_path` and act on the decision.
///
/// On [`RelaunchDecision::SwapTo`] (sentinel exit + valid manifest) it swaps the
/// freshly built binary over `daemon_path`, deletes the manifest so the next
/// exit does not loop, and arms a rollback watch. Every other decision is the
/// historical restart-same path. `decide_relaunch` never returns `Rollback`
/// (those are driven by [`fold_swap_watch`]), so that variant is a safe no-op.
fn apply_relaunch_decision(
    daemon_path: &Path,
    manifest_path: &Path,
    code: i32,
    watch: &mut Option<SwapWatch>,
) {
    let manifest = match load_manifest(manifest_path) {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, "supervisor: could not read relaunch manifest; treating as crash-restart");
            None
        }
    };
    let valid = manifest.as_ref().is_some_and(new_binary_valid);
    let RelaunchDecision::SwapTo(new_binary) = decide_relaunch(code, manifest.as_ref(), valid) else {
        // RestartSame (and the never-emitted Rollback): today's behaviour — just
        // restart the current binary.
        return;
    };

    let backup = backup_path_for(daemon_path);
    match perform_swap(daemon_path, &new_binary, &backup) {
        Ok(()) => {
            info!(
                new = %new_binary.display(),
                backup = %backup.display(),
                "supervisor: swapped in freshly self-built binary"
            );
            // Delete the manifest after a successful swap so the next exit does
            // not loop back into another swap of the same binary.
            let _ = std::fs::remove_file(manifest_path);
            *watch = Some(SwapWatch {
                swapped_at: Instant::now(),
                crashes_since_swap: 0,
                backup_path: backup,
            });
        }
        Err(e) => {
            error!(error = %e, new = %new_binary.display(), "supervisor: binary swap FAILED; restarting current binary");
            // Don't leave a manifest that would retry a doomed swap.
            let _ = std::fs::remove_file(manifest_path);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use origin_supervisor::resume_token::ResumeToken;
    use std::sync::Mutex;

    /// `set_var`/`remove_var` are process-global; serialize the env-mutating
    /// tests so they cannot interleave.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn token(id: &str) -> ResumeToken {
        ResumeToken {
            session_id: id.to_string(),
            last_turn: 1,
            cas_handle_root: [0u8; 32],
            pending_tool_calls: Vec::new(),
            plan_seq: 0,
            goal: None,
            detached_at_unix: None,
            memory_estimate_bytes: None,
        }
    }

    #[test]
    fn resolve_resume_dir_derives_from_origin_db() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("sessions.db");
        std::env::set_var("ORIGIN_DB", &db);
        let dir = resolve_resume_dir();
        std::env::remove_var("ORIGIN_DB");
        // Mirrors SessionStore::resume_dir(): <dir-of-db>/resume.
        assert_eq!(dir, tmp.path().join("resume"));
    }

    #[test]
    fn enumeration_finds_persisted_tokens() {
        // Two persisted tokens in a temp resume dir are both enumerated. This
        // is the decision the detached replay thread makes before any IPC: a
        // non-empty result means "replay", empty means "no-op".
        let tmp = tempfile::tempdir().unwrap();
        token("alpha").save(tmp.path()).unwrap();
        token("beta").save(tmp.path()).unwrap();

        let mut loaded = ResumeToken::load_all(tmp.path()).unwrap();
        loaded.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].session_id, "alpha");
        assert_eq!(loaded[1].session_id, "beta");
        assert!(!loaded.is_empty(), "non-empty => replay path is taken");
    }

    #[test]
    fn empty_dir_is_noop() {
        // An empty (no-token) resume dir enumerates to nothing, so the replay
        // thread short-circuits without opening an IPC connection — the
        // default-safe path that is byte-identical to pre-wiring behaviour.
        let tmp = tempfile::tempdir().unwrap();
        let loaded = ResumeToken::load_all(tmp.path()).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn missing_dir_is_noop() {
        // A resume dir that never existed (no detached session ever) is also a
        // clean no-op rather than an error.
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let loaded = ResumeToken::load_all(&missing).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn replay_to_dead_endpoint_is_bounded_error_not_panic() {
        // drive_resume_replay against an endpoint nothing is listening on must
        // return an Err (so the caller logs+swallows it) rather than hang
        // forever or panic. Use a low attempt budget via a bogus path.
        let tokens = vec![token("x")];
        // A path no daemon binds. On Windows this is a non-existent pipe; on
        // unix a socket path that was never created.
        #[cfg(windows)]
        let endpoint = r"\\.\pipe\origin-supervisor-test-nonexistent".to_string();
        #[cfg(unix)]
        let endpoint = {
            let tmp = tempfile::tempdir().unwrap();
            tmp.path().join("nonexistent.sock").to_string_lossy().into_owned()
        };
        // Use a tiny retry budget so the test terminates fast; assert only
        // that an unreachable endpoint yields an error (so the caller
        // logs+swallows it) rather than a hang or panic.
        let res =
            drive_resume_replay_with(tokens, &endpoint, 2, std::time::Duration::from_millis(5));
        assert!(res.is_err(), "unreachable endpoint must yield an error, not success");
    }
}
