// SPDX-License-Identifier: Apache-2.0
//! Self-dev **relaunch** plumbing — the pure supervisor-side logic for swapping
//! the running daemon binary for a freshly self-built one and rolling back if
//! the new binary crashes.

// `RelaunchManifest`, `RelaunchDecision`, `decide_relaunch`, etc. intentionally
// carry the module's name: they are the crate's public *relaunch* vocabulary the
// supervisor binary consumes via `origin_supervisor::Relaunch*`, so the
// repetition is by design (matching the sibling `lifecycle` module).
#![allow(clippy::module_name_repetitions)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Exit code the daemon uses to signal "I have written a relaunch manifest and
/// want the supervisor to swap me for the new binary" (rather than a crash).
///
/// Chosen to sit outside the common signal-derived (`128 + signo`) range and
/// the conventional `0..=2` success/usage range so it is unambiguous.
pub const SELFDEV_RELAUNCH_EXIT_CODE: i32 = 86;

/// The on-disk handoff the daemon writes (as `relaunch.json`) immediately
/// before exiting with [`SELFDEV_RELAUNCH_EXIT_CODE`].
///
/// The three `snake_case` fields are the *exact* contract shared with the
/// `origin-selfdev` crate that produces the file:
///
/// - `new_binary_path` — the freshly self-built daemon to swap in.
/// - `previous_binary_path` — the binary currently running (so a rollback can
///   restore it).
/// - `generation` — the monotonic successor counter mirrored from the self-dev
///   reload context so the supervisor can detect restart storms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelaunchManifest {
    /// Path to the freshly built daemon binary to swap in.
    pub new_binary_path: PathBuf,
    /// Path to the binary currently running, kept for rollback.
    pub previous_binary_path: PathBuf,
    /// Monotonic successor counter for this relaunch.
    pub generation: u64,
}

/// Load the relaunch manifest at `path`.
///
/// Returns `Ok(None)` when the file does not exist (the common case: the daemon
/// exited for some reason other than a self-dev relaunch and never wrote one).
/// Returns `Err` only when the file exists but cannot be read or parsed.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read, or if its contents
/// are not a valid [`RelaunchManifest`] JSON document.
pub fn load_manifest(path: &Path) -> Result<Option<RelaunchManifest>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("reading relaunch manifest at {}", path.display()));
        }
    };
    let manifest: RelaunchManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing relaunch manifest at {}", path.display()))?;
    Ok(Some(manifest))
}

/// What the supervisor should do after the daemon child exits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelaunchDecision {
    /// Swap the running binary for the one at this path, then start it. Emitted
    /// only when the daemon exited with [`SELFDEV_RELAUNCH_EXIT_CODE`], a valid
    /// manifest was present, and the new binary passed validation.
    SwapTo(PathBuf),
    /// Restart the *current* binary unchanged — the normal crash-restart path,
    /// and the safe fallback whenever a relaunch cannot be trusted.
    RestartSame,
    /// Restore (and restart) the previous binary at this path because a freshly
    /// swapped binary is crash-looping. Produced by the supervisor's restart
    /// loop via [`should_rollback`], not by [`decide_relaunch`].
    Rollback(PathBuf),
}

/// Decide what to do after the daemon child exits with `child_exit_code`.
///
/// Returns [`RelaunchDecision::SwapTo`] **only** when all three hold:
/// the exit code equals [`SELFDEV_RELAUNCH_EXIT_CODE`], a `manifest` is present,
/// and `new_binary_valid` is `true`. In every other case it returns
/// [`RelaunchDecision::RestartSame`] — the conservative path that keeps the
/// known-good binary running.
#[must_use]
pub fn decide_relaunch(
    child_exit_code: i32,
    manifest: Option<&RelaunchManifest>,
    new_binary_valid: bool,
) -> RelaunchDecision {
    match manifest {
        Some(m) if child_exit_code == SELFDEV_RELAUNCH_EXIT_CODE && new_binary_valid => {
            RelaunchDecision::SwapTo(m.new_binary_path.clone())
        }
        _ => RelaunchDecision::RestartSame,
    }
}

/// Swap the binary at `current` for the one at `new`, first backing up the
/// existing `current` to `backup`.
///
/// The replacement is performed atomically with respect to `current`: the new
/// bytes are staged into a temporary file in the same directory as `current`
/// and then `rename`d over it, so a reader either sees the whole old binary or
/// the whole new one — never a half-written file.
///
/// Ordering is chosen so a missing/unreadable `new` binary leaves `current`
/// untouched: the new bytes are staged *before* `current` is modified, and the
/// backup is only taken once staging has succeeded.
///
/// # Errors
///
/// Returns an [`std::io::Error`] if `new` cannot be read, if the backup copy
/// fails, or if staging/renaming the replacement fails.
pub fn perform_swap(current: &Path, new: &Path, backup: &Path) -> std::io::Result<()> {
    // 1) Stage the new bytes next to `current` first. If `new` is missing or
    //    unreadable this fails here, before `current` or `backup` are touched.
    let staged = staging_path(current);
    // `copy` truncates/creates the destination, so a stale staging file (from a
    // crashed prior swap) is harmless.
    std::fs::copy(new, &staged)?;

    // 2) Back up the currently-installed binary so a rollback can restore it.
    //    `copy` overwrites any pre-existing (stale) backup.
    if let Err(err) = std::fs::copy(current, backup) {
        // Don't leave the staging file behind on failure.
        let _ = std::fs::remove_file(&staged);
        return Err(err);
    }

    // 3) Atomically move the staged bytes over `current`.
    if let Err(err) = std::fs::rename(&staged, current) {
        let _ = std::fs::remove_file(&staged);
        return Err(err);
    }
    Ok(())
}

/// Decide whether a freshly swapped binary is crash-looping badly enough to
/// roll back to the previous one.
///
/// Returns `true` when, since the relaunch, the new binary has crashed at least
/// `threshold` times (`crashes >= threshold`) and that streak is still inside
/// the observation `window` (`elapsed_since_relaunch < window`). The window is
/// half-open: a streak that only reaches the threshold *at or after* `window`
/// has elapsed no longer counts, so a binary that survives the window is
/// considered healthy.
///
/// A `threshold` of `0` always returns `false` — rolling back without any crash
/// would be nonsensical — guarding against an accidental "always roll back".
#[must_use]
pub fn should_rollback(
    crashes: u32,
    elapsed_since_relaunch: std::time::Duration,
    threshold: u32,
    window: std::time::Duration,
) -> bool {
    threshold >= 1 && crashes >= threshold && elapsed_since_relaunch < window
}

/// What the supervisor's post-swap *watch* should do after the freshly swapped
/// binary exits, given the crash streak and how long it has been running.
///
/// Produced by [`watch_outcome`] from the pure inputs; the loop then performs
/// the corresponding side effect (restore the backup, clear the watch, or keep
/// counting). Decoupled from the loop so the policy is unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchOutcome {
    /// The swapped binary is crash-looping inside the window: restore the
    /// previous binary from the backup and stop trying to swap.
    RollBack,
    /// The swapped binary survived the observation window: it is healthy, so
    /// stop watching (and the caller drops the manifest).
    ClearStable,
    /// Still inside the window and below threshold: keep the watch active.
    KeepWatching,
}

/// Decide the post-swap watch outcome from the crash streak and elapsed time.
///
/// `crashes_since_swap` counts non-sentinel exits observed since the swap.
/// Rolls back when [`should_rollback`] fires; otherwise, once the observation
/// `window` has fully elapsed the swapped binary is deemed stable
/// ([`WatchOutcome::ClearStable`]); inside the window with too few crashes it
/// keeps watching.
#[must_use]
pub fn watch_outcome(
    crashes_since_swap: u32,
    elapsed_since_swap: std::time::Duration,
    threshold: u32,
    window: std::time::Duration,
) -> WatchOutcome {
    if should_rollback(crashes_since_swap, elapsed_since_swap, threshold, window) {
        WatchOutcome::RollBack
    } else if elapsed_since_swap >= window {
        WatchOutcome::ClearStable
    } else {
        WatchOutcome::KeepWatching
    }
}

/// Resolve the platform "local data" directory WITHOUT depending on the `dirs`
/// crate (the supervisor must not pull a new dependency), mirroring
/// `dirs::data_local_dir()`'s conventions via the injected `lookup`:
///
/// - Windows: `%LOCALAPPDATA%`.
/// - macOS: `$HOME/Library/Application Support`.
/// - other Unix: `$XDG_DATA_HOME` if set+non-empty, else `$HOME/.local/share`.
///
/// `lookup` is injected so the resolution is unit-testable without mutating the
/// process environment; [`data_local_dir`] is the thin wrapper that reads the
/// real environment.
fn data_local_dir_with<F>(lookup: F) -> Option<PathBuf>
where
    F: Fn(&str) -> Option<std::ffi::OsString>,
{
    let non_empty = |key: &str| lookup(key).filter(|v| !v.is_empty());

    #[cfg(windows)]
    {
        non_empty("LOCALAPPDATA").map(PathBuf::from)
    }

    #[cfg(target_os = "macos")]
    {
        non_empty("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        non_empty("XDG_DATA_HOME").map(PathBuf::from).or_else(|| {
            non_empty("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
        })
    }
}

/// The platform local-data directory, mirroring `dirs::data_local_dir()`, read
/// from the real process environment. `None` when it cannot be resolved (e.g.
/// `%LOCALAPPDATA%`/`$HOME` unset), in which case the caller falls back to the
/// current directory so a relaunch manifest is still looked for somewhere
/// deterministic (matching the daemon's `unwrap_or_else(|| ".".into())`).
fn data_local_dir() -> Option<PathBuf> {
    data_local_dir_with(|k| std::env::var_os(k))
}

/// The relaunch-manifest path the daemon writes and the supervisor reads:
/// `<state_dir>/origin/selfdev/relaunch.json`. Mirrors the daemon's
/// `origin_daemon::selfdev::relaunch_store_path` layout exactly.
fn relaunch_manifest_path(state_dir: &Path) -> PathBuf {
    state_dir.join("origin").join("selfdev").join("relaunch.json")
}

/// The relaunch-manifest path under the resolved local-data dir (falling back to
/// the current directory, matching the daemon's fallback), so the supervisor
/// reads the same file the daemon wrote.
#[must_use]
pub fn default_relaunch_manifest_path() -> PathBuf {
    let state_dir = data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    relaunch_manifest_path(&state_dir)
}

/// Path of the in-place staging file used by [`perform_swap`]: a sibling of
/// `current` so the final `rename` stays within one directory (a requirement
/// for atomic renames on every platform).
fn staging_path(current: &Path) -> PathBuf {
    let mut name = current.file_name().map_or_else(
        || std::ffi::OsString::from("origin-daemon"),
        std::ffi::OsStr::to_os_string,
    );
    name.push(".swap-staging");
    current.with_file_name(name)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn sentinel_exit_code_is_86() {
        assert_eq!(SELFDEV_RELAUNCH_EXIT_CODE, 86);
    }

    #[test]
    fn watch_outcome_rolls_back_at_threshold_inside_window() {
        use std::time::Duration;
        let window = Duration::from_secs(5);
        // 2 crashes within the 5s window with threshold 2 ⇒ roll back.
        assert_eq!(
            watch_outcome(2, Duration::from_secs(1), 2, window),
            WatchOutcome::RollBack
        );
    }

    #[test]
    fn watch_outcome_keeps_watching_below_threshold_inside_window() {
        use std::time::Duration;
        let window = Duration::from_secs(5);
        assert_eq!(
            watch_outcome(1, Duration::from_secs(1), 2, window),
            WatchOutcome::KeepWatching
        );
    }

    #[test]
    fn watch_outcome_clears_stable_after_window() {
        use std::time::Duration;
        let window = Duration::from_secs(5);
        // Survived the window (even with one crash) ⇒ healthy, stop watching.
        assert_eq!(
            watch_outcome(1, Duration::from_secs(5), 2, window),
            WatchOutcome::ClearStable
        );
        assert_eq!(
            watch_outcome(0, Duration::from_secs(10), 2, window),
            WatchOutcome::ClearStable
        );
    }

    #[test]
    fn relaunch_manifest_path_uses_conventional_layout() {
        // The daemon writes `<data_local_dir>/origin/selfdev/relaunch.json`; the
        // supervisor must read the identical path.
        let p = relaunch_manifest_path(Path::new("/state"));
        assert!(
            p.ends_with(Path::new("origin/selfdev/relaunch.json")),
            "got {p:?}"
        );
        assert!(p.starts_with("/state"), "got {p:?}");
    }

    #[cfg(windows)]
    #[test]
    fn data_local_dir_from_localappdata_on_windows() {
        // Mirrors `dirs::data_local_dir()` == %LOCALAPPDATA% on Windows.
        let got = data_local_dir_with(|k| match k {
            "LOCALAPPDATA" => Some(std::ffi::OsString::from("C:\\Users\\u\\AppData\\Local")),
            _ => None,
        });
        assert_eq!(got, Some(PathBuf::from("C:\\Users\\u\\AppData\\Local")));
    }

    #[cfg(windows)]
    #[test]
    fn data_local_dir_none_when_localappdata_unset_on_windows() {
        let got = data_local_dir_with(|_| None);
        assert_eq!(got, None);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn data_local_dir_prefers_xdg_then_home_on_linux() {
        // XDG_DATA_HOME wins when set.
        let got = data_local_dir_with(|k| match k {
            "XDG_DATA_HOME" => Some(std::ffi::OsString::from("/xdg/data")),
            "HOME" => Some(std::ffi::OsString::from("/home/u")),
            _ => None,
        });
        assert_eq!(got, Some(PathBuf::from("/xdg/data")));

        // Falls back to $HOME/.local/share when XDG is unset/empty.
        let got = data_local_dir_with(|k| match k {
            "HOME" => Some(std::ffi::OsString::from("/home/u")),
            _ => None,
        });
        assert_eq!(got, Some(PathBuf::from("/home/u/.local/share")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn data_local_dir_uses_application_support_on_macos() {
        let got = data_local_dir_with(|k| match k {
            "HOME" => Some(std::ffi::OsString::from("/Users/u")),
            _ => None,
        });
        assert_eq!(
            got,
            Some(PathBuf::from("/Users/u/Library/Application Support"))
        );
    }

    #[test]
    fn load_manifest_reads_three_snake_case_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relaunch.json");
        // Exactly the shape the selfdev crate writes.
        let json = r#"{
            "new_binary_path": "/opt/origin/bin/origin-daemon.next",
            "previous_binary_path": "/opt/origin/bin/origin-daemon",
            "generation": 7
        }"#;
        std::fs::write(&path, json).unwrap();

        let loaded = load_manifest(&path).unwrap().expect("manifest present");
        assert_eq!(
            loaded.new_binary_path,
            std::path::PathBuf::from("/opt/origin/bin/origin-daemon.next")
        );
        assert_eq!(
            loaded.previous_binary_path,
            std::path::PathBuf::from("/opt/origin/bin/origin-daemon")
        );
        assert_eq!(loaded.generation, 7);
    }

    #[test]
    fn load_manifest_absent_file_is_ok_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        assert!(load_manifest(&path).unwrap().is_none());
    }

    fn manifest() -> RelaunchManifest {
        RelaunchManifest {
            new_binary_path: PathBuf::from("/n/origin-daemon.next"),
            previous_binary_path: PathBuf::from("/n/origin-daemon"),
            generation: 3,
        }
    }

    // (exit_code, manifest?, new_binary_valid, expected) row for the
    // `decide_relaunch` truth table.
    struct Case {
        name: &'static str,
        exit: i32,
        manifest: Option<RelaunchManifest>,
        valid: bool,
        expect_swap: bool,
    }

    #[test]
    fn decide_relaunch_table() {
        let m = manifest();
        let cases = [
            Case {
                name: "sentinel + manifest + valid -> SwapTo",
                exit: SELFDEV_RELAUNCH_EXIT_CODE,
                manifest: Some(m.clone()),
                valid: true,
                expect_swap: true,
            },
            Case {
                name: "sentinel + manifest + INVALID -> RestartSame",
                exit: SELFDEV_RELAUNCH_EXIT_CODE,
                manifest: Some(m.clone()),
                valid: false,
                expect_swap: false,
            },
            Case {
                name: "sentinel + NO manifest -> RestartSame",
                exit: SELFDEV_RELAUNCH_EXIT_CODE,
                manifest: None,
                valid: true,
                expect_swap: false,
            },
            Case {
                name: "non-sentinel + manifest + valid -> RestartSame (crash)",
                exit: 1,
                manifest: Some(m.clone()),
                valid: true,
                expect_swap: false,
            },
            Case {
                name: "clean exit (0) + manifest + valid -> RestartSame",
                exit: 0,
                manifest: Some(m.clone()),
                valid: true,
                expect_swap: false,
            },
        ];
        for c in cases {
            let decision = decide_relaunch(c.exit, c.manifest.as_ref(), c.valid);
            match (&decision, c.expect_swap) {
                (RelaunchDecision::SwapTo(p), true) => {
                    assert_eq!(p, &m.new_binary_path, "{}", c.name);
                }
                (RelaunchDecision::RestartSame, false) => {}
                other => panic!("{}: unexpected {other:?}", c.name),
            }
        }
    }

    #[test]
    fn perform_swap_backs_up_old_and_installs_new() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("origin-daemon");
        let new = dir.path().join("origin-daemon.next");
        let backup = dir.path().join("origin-daemon.bak");

        std::fs::write(&current, b"OLD-BINARY-BYTES").unwrap();
        std::fs::write(&new, b"NEW-BINARY-BYTES").unwrap();

        perform_swap(&current, &new, &backup).unwrap();

        // Backup holds the old bytes verbatim.
        assert_eq!(std::fs::read(&backup).unwrap(), b"OLD-BINARY-BYTES");
        // Current now holds the new bytes.
        assert_eq!(std::fs::read(&current).unwrap(), b"NEW-BINARY-BYTES");
    }

    #[test]
    fn perform_swap_overwrites_existing_backup() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("d");
        let new = dir.path().join("d.next");
        let backup = dir.path().join("d.bak");

        std::fs::write(&current, b"V2").unwrap();
        std::fs::write(&new, b"V3").unwrap();
        std::fs::write(&backup, b"STALE-V1").unwrap(); // pre-existing backup

        perform_swap(&current, &new, &backup).unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), b"V2");
        assert_eq!(std::fs::read(&current).unwrap(), b"V3");
    }

    #[test]
    fn perform_swap_missing_new_errors_and_preserves_current() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("d");
        let new = dir.path().join("absent.next");
        let backup = dir.path().join("d.bak");
        std::fs::write(&current, b"KEEP-ME").unwrap();

        assert!(perform_swap(&current, &new, &backup).is_err());
        // Current must be untouched when the new binary is missing.
        assert_eq!(std::fs::read(&current).unwrap(), b"KEEP-ME");
    }

    #[test]
    fn rollback_variant_constructible() {
        // `Rollback` is produced by the supervisor's restart loop (not by
        // `decide_relaunch`); exercise it so it is not dead code.
        let prev = PathBuf::from("/n/origin-daemon");
        let d = RelaunchDecision::Rollback(prev.clone());
        match d {
            RelaunchDecision::Rollback(p) => assert_eq!(p, prev),
            other => panic!("expected Rollback, got {other:?}"),
        }
    }

    #[test]
    fn should_rollback_table() {
        use std::time::Duration;
        let window = Duration::from_secs(60);
        let threshold = 3;
        // (name, crashes, elapsed, expected)
        let cases = [
            ("below threshold, in window", 2, Duration::from_secs(10), false),
            ("at threshold, in window", 3, Duration::from_secs(10), true),
            ("above threshold, in window", 5, Duration::from_secs(59), true),
            ("at threshold but past window", 3, Duration::from_secs(60), false),
            ("at threshold but well past window", 9, Duration::from_secs(600), false),
            ("zero crashes", 0, Duration::from_secs(1), false),
            ("at threshold at window edge (just under)", 3, Duration::from_millis(59_999), true),
        ];
        for (name, crashes, elapsed, expected) in cases {
            assert_eq!(
                should_rollback(crashes, elapsed, threshold, window),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn should_rollback_zero_threshold_never_triggers() {
        use std::time::Duration;
        // A threshold of 0 is meaningless ("roll back after no crashes"); guard
        // against it so a swapped binary is never rolled back without a crash.
        assert!(!should_rollback(0, Duration::from_secs(1), 0, Duration::from_secs(60)));
        assert!(!should_rollback(5, Duration::from_secs(1), 0, Duration::from_secs(60)));
    }
}
