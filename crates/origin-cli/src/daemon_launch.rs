// SPDX-License-Identifier: Apache-2.0
//! Pure launch-decision helpers for [`crate`]'s daemon bring-up.
//!
//! `ensure_daemon_running` now defaults to routing the daemon through the
//! `origin-supervisor` binary, which owns + restarts `origin-daemon` and — most
//! importantly — consumes the self-dev relaunch sentinel (exit code 86) to
//! hot-swap a freshly built binary. The user can opt back into the legacy direct
//! spawn with `ORIGIN_NO_SUPERVISOR=1`, and we also fall back to a direct spawn
//! when the supervisor binary is not available so launch never fails for lack of
//! it.
//!
//! The decision and the supervisor argv are split out here as pure functions so
//! they are deterministically unit-testable without spawning any processes.

use std::ffi::{OsStr, OsString};

/// Which binary [`crate`]'s `ensure_daemon_running` spawns to bring the daemon up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launcher {
    /// Spawn `origin-supervisor`, which owns + restarts `origin-daemon` and
    /// consumes the self-dev relaunch sentinel (exit 86) to hot-swap the binary.
    /// This is the default.
    Supervisor,
    /// Spawn `origin-daemon` directly (legacy path; no self-dev hot-reload, no
    /// crash-restart). Used when the user opts out or the supervisor is absent.
    DaemonDirect,
}

/// Decide which launcher to use.
///
/// Supervised launch is the DEFAULT — we route the daemon through
/// `origin-supervisor` unless the user opted out (`no_supervisor`, i.e.
/// `ORIGIN_NO_SUPERVISOR=1`) or the supervisor binary is unavailable
/// (`!supervisor_available`), in which case we spawn the daemon directly so
/// bring-up never fails for lack of the supervisor.
#[must_use]
pub const fn select_launcher(no_supervisor: bool, supervisor_available: bool) -> Launcher {
    if no_supervisor || !supervisor_available {
        Launcher::DaemonDirect
    } else {
        Launcher::Supervisor
    }
}

/// True when `ORIGIN_NO_SUPERVISOR=1` opts out of supervised launch.
#[must_use]
pub fn no_supervisor_env() -> bool {
    std::env::var("ORIGIN_NO_SUPERVISOR").as_deref() == Ok("1")
}

/// The argv passed to `origin-supervisor`: point it at the resolved daemon
/// binary via `--daemon-path <path>`.
///
/// No daemon arguments are forwarded — provider/account propagate via the
/// supervisor process environment (`ORIGIN_PROVIDER`/`ORIGIN_ACCOUNT`), which the
/// daemon child inherits when the supervisor spawns it.
#[must_use]
pub fn supervisor_args(daemon_path: &OsStr) -> Vec<OsString> {
    vec![OsString::from("--daemon-path"), daemon_path.to_os_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supervised_is_the_default() {
        // No opt-out, supervisor present => supervised launch.
        assert_eq!(select_launcher(false, true), Launcher::Supervisor);
    }

    #[test]
    fn opt_out_env_forces_direct() {
        // ORIGIN_NO_SUPERVISOR=1 => direct daemon spawn even when available.
        assert_eq!(select_launcher(true, true), Launcher::DaemonDirect);
    }

    #[test]
    fn missing_supervisor_falls_back_to_direct() {
        // Never fail to launch: no supervisor binary => direct, opt-out or not.
        assert_eq!(select_launcher(false, false), Launcher::DaemonDirect);
        assert_eq!(select_launcher(true, false), Launcher::DaemonDirect);
    }

    #[test]
    fn supervisor_args_point_at_daemon_path() {
        let args = supervisor_args(OsStr::new("/opt/origin/origin-daemon"));
        assert_eq!(
            args,
            vec![
                OsString::from("--daemon-path"),
                OsString::from("/opt/origin/origin-daemon"),
            ]
        );
    }
}
