// SPDX-License-Identifier: Apache-2.0
//! `origin-supervisor` — restarts `origin-daemon` on crash; coordinates session
//! resume tokens across the restart.

use clap::Parser;
use origin_supervisor::relaunch::{
    decide_relaunch, default_relaunch_manifest_path, load_manifest, perform_swap, watch_outcome,
    RelaunchDecision, RelaunchManifest, WatchOutcome, SELFDEV_RELAUNCH_EXIT_CODE,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

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
