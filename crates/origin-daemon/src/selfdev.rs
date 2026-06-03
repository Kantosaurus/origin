// SPDX-License-Identifier: Apache-2.0
//! Daemon-side control plane for binary self-development.
//!
//! The pure orchestration state machine lives in [`origin_selfdev`]; this module
//! is the daemon's thin adapter that holds the process-global
//! [`SelfDevDriver`](origin_selfdev::SelfDevDriver), an operator-approval flag,
//! and the real effect implementations the machine drives:
//!
//! - [`ProcessGitRunner`] — a real [`GitRunner`](origin_vcs::GitRunner) that
//!   shells `git` for the shadow-history checkpoint/rollback.
//! - [`ShadowRollback`] — a [`Rollback`](origin_selfdev::Rollback) that restores
//!   the working tree from the pre-edit shadow checkpoint.
//! - [`ApprovalAuthority`] — a [`RestartAuthority`](origin_selfdev::RestartAuthority)
//!   that grants a supervised restart iff the operator sent `SelfDevApprove` for
//!   the current job.
//!
//! # Default-off by construction
//!
//! The entire control plane is gated on `ORIGIN_SELFDEV=1`. [`enabled`] is the
//! single gate: when unset, [`global`] constructs nothing and every self-dev IPC
//! verb returns a clear "self-dev disabled" message. This mirrors the ambient
//! `IDLE_TRACKER` / [`crate::supervisor`] `STATE` `OnceLock` pattern.
//!
//! # Safety (preserved exactly from the state machine's invariants)
//!
//! - A checkpoint is ALWAYS taken before any edit (the daemon handler takes it
//!   before driving the edit step), so the rollback has a target.
//! - Build AND test must both pass before `AwaitingRestart`; the machine has no
//!   other inbound edge.
//! - A restart is only ever granted via `request_restart` consulting
//!   [`ApprovalAuthority`] — never by calling `RestartGranted` directly.
//! - A tripped storm guard or a failed rollback is operator-escalation: the
//!   daemon logs and stops, it does not retry.
//! - The ACTUAL process relaunch/`exec` is intentionally NOT performed here. The
//!   daemon authorizes the restart, persists the [`ReloadContext`], writes the
//!   resume token, and logs "ready to relaunch" — the real relaunch is a
//!   TODO-logged hook into origin-supervisor's resume-restart path.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use origin_selfdev::{
    ReloadContext, RestartAuthority, RestartDecision, Rollback, RollbackOutcome, SelfDevConfig,
    SelfDevDriver,
};
use origin_vcs::{GitRunner, RestoreMode, ShadowGit, VcsError};

/// Env gate: the whole self-dev control plane is inert unless this is `1`.
const SELFDEV_ENV: &str = "ORIGIN_SELFDEV";

/// Default storm-guard ceiling (consecutive failed generations before self-dev
/// refuses new jobs until [`SelfDevDriver::reset_storm_guard`]).
const MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// Process-global self-dev state. `None` until first use AND only when
/// `ORIGIN_SELFDEV=1`; otherwise [`global`] returns `None` and the daemon is
/// byte-identical to a build without this feature.
static STATE: OnceLock<Option<SelfDevState>> = OnceLock::new();

/// The daemon's live self-dev state: the orchestration driver plus the operator
/// approval flag the [`ApprovalAuthority`] reads.
pub struct SelfDevState {
    /// The pure orchestration state machine. A `std::sync::Mutex` (not tokio):
    /// every critical section is a quick, non-await fold; the blocking build/test
    /// runs are done OUTSIDE this lock on `spawn_blocking`.
    driver: Mutex<SelfDevDriver>,
    /// Set by `SelfDevApprove`; read (and cleared) by [`ApprovalAuthority`] when
    /// authorizing the in-flight restart. A single shared flag is sufficient
    /// because the driver runs exactly one job at a time.
    approved: AtomicBool,
}

impl SelfDevState {
    /// Lock the driver, recovering from a poisoned mutex rather than panicking.
    pub fn driver(&self) -> MutexGuard<'_, SelfDevDriver> {
        self.driver.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Mark the in-flight restart as operator-approved. The next
    /// [`SelfDevDriver::request_restart`] then returns `Grant`.
    pub fn approve(&self) {
        self.approved.store(true, Ordering::Release);
    }

    /// Clear the approval flag (called by the authority after a grant so the
    /// next job must be approved afresh).
    fn take_approval(&self) -> bool {
        self.approved.swap(false, Ordering::AcqRel)
    }
}

/// `true` iff the self-dev control plane is enabled (`ORIGIN_SELFDEV=1`).
#[must_use]
pub fn enabled() -> bool {
    std::env::var(SELFDEV_ENV).as_deref() == Ok("1")
}

/// The process-global self-dev state, or `None` when disabled.
///
/// Constructs the driver lazily on first call when `ORIGIN_SELFDEV=1`; returns
/// `None` (constructing nothing) otherwise. Idempotent.
#[must_use]
pub fn global() -> Option<&'static SelfDevState> {
    STATE
        .get_or_init(|| {
            if enabled() {
                Some(SelfDevState {
                    driver: Mutex::new(SelfDevDriver::new(SelfDevConfig {
                        max_consecutive_failures: MAX_CONSECUTIVE_FAILURES,
                    })),
                    approved: AtomicBool::new(false),
                })
            } else {
                None
            }
        })
        .as_ref()
}

/// The operator-facing message returned for any self-dev verb when disabled.
#[must_use]
pub const fn disabled_message() -> &'static str {
    "self-dev disabled (set ORIGIN_SELFDEV=1)"
}

/// A real [`GitRunner`] that shells out to the `git` binary in a working dir.
///
/// Only used for the shadow-history checkpoint and rollback; like
/// [`origin_selfdev::CargoRunner`], it is the only part of the daemon self-dev
/// wiring that performs real process I/O, and it degrades gracefully (a spawn
/// failure or non-zero exit becomes [`VcsError::Git`], never a panic).
pub struct ProcessGitRunner {
    /// Directory the `git` invocations run in (the workspace root).
    workspace_dir: std::path::PathBuf,
}

impl ProcessGitRunner {
    /// Construct a runner rooted at `workspace_dir`.
    #[must_use]
    pub fn new(workspace_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace_dir: workspace_dir.into(),
        }
    }
}

impl GitRunner for ProcessGitRunner {
    fn run(&self, args: &[&str]) -> Result<String, VcsError> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&self.workspace_dir)
            .output()
            .map_err(|e| VcsError::Git(format!("failed to spawn git: {e}")))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(VcsError::Git(stderr.trim().to_string()))
        }
    }
}

/// A [`Rollback`] that restores the working tree from a pre-edit shadow
/// checkpoint.
///
/// SAFETY RULE (b): any build/test failure rolls back to the same binary.
/// Scoped to the job's `target_paths` when present, else the whole working tree.
pub struct ShadowRollback<'a> {
    /// The shadow-git handle layered over the workspace.
    shadow: ShadowGit<'a>,
    /// The checkpoint id taken BEFORE the edit (the rollback target).
    checkpoint_id: String,
}

impl<'a> ShadowRollback<'a> {
    /// Construct a rollback against `checkpoint_id` in `shadow`.
    #[must_use]
    pub fn new(shadow: ShadowGit<'a>, checkpoint_id: impl Into<String>) -> Self {
        Self {
            shadow,
            checkpoint_id: checkpoint_id.into(),
        }
    }
}

impl Rollback for ShadowRollback<'_> {
    fn rollback(&self, job: &origin_selfdev::BuildJob) -> Result<RollbackOutcome, String> {
        // Scope to the job's declared paths when present (a tighter restore),
        // otherwise restore the whole working tree from the checkpoint.
        let mode = if job.target_paths.is_empty() {
            RestoreMode::WorkingTree
        } else {
            RestoreMode::Files(job.target_paths.clone())
        };
        self.shadow
            .restore(&self.checkpoint_id, &mode)
            .map(|()| RollbackOutcome::Restored)
            .map_err(|e| e.to_string())
    }
}

/// A [`RestartAuthority`] that grants the supervised restart iff the operator
/// sent `SelfDevApprove` for the current job.
///
/// SAFETY RULE (c): restart is explicitly granted, never automatic. The approval
/// flag is consumed on a grant so each job requires a fresh approval.
pub struct ApprovalAuthority<'a> {
    state: &'a SelfDevState,
}

impl<'a> ApprovalAuthority<'a> {
    /// Construct an authority reading `state`'s approval flag.
    #[must_use]
    pub const fn new(state: &'a SelfDevState) -> Self {
        Self { state }
    }
}

impl RestartAuthority for ApprovalAuthority<'_> {
    fn authorize(&self, _ctx: &ReloadContext) -> RestartDecision {
        if self.state.take_approval() {
            RestartDecision::Grant
        } else {
            RestartDecision::Deny("operator approval required (send SelfDevApprove)".to_string())
        }
    }
}

/// The daemon state directory under which self-dev persists its reload context.
///
/// Mirrors `main`'s trace-dir convention (`data_local_dir()/origin`), falling
/// back to the current directory so a missing data dir never aborts. The reload
/// store path is `<state_dir>/selfdev/reload.json`.
#[must_use]
pub fn reload_store_path() -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("origin")
        .join("selfdev")
        .join("reload.json")
}

/// The shadow-git directory self-dev checkpoints into, under the state dir so it
/// never pollutes the user's real `.git`.
#[must_use]
pub fn shadow_git_dir() -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("origin")
        .join("selfdev")
        .join("shadow.git")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use origin_selfdev::BuildJob;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// Mock git runner so the rollback impl is testable offline.
    #[derive(Default)]
    struct MockGit {
        replies: RefCell<VecDeque<Result<String, VcsError>>>,
    }
    impl MockGit {
        fn scripted(replies: Vec<Result<String, VcsError>>) -> Self {
            Self {
                replies: RefCell::new(replies.into_iter().collect()),
            }
        }
    }
    impl GitRunner for MockGit {
        fn run(&self, _args: &[&str]) -> Result<String, VcsError> {
            self.replies
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Ok(String::new()))
        }
    }

    #[test]
    fn disabled_when_env_unset() {
        // We do not mutate the process env here (parallel-safe). A clean test
        // shell never sets ORIGIN_SELFDEV, so `enabled()` is false and `global()`
        // yields None — the default-off contract.
        if std::env::var(SELFDEV_ENV).is_err() {
            assert!(!enabled(), "self-dev must default to off");
            assert!(global().is_none(), "no state constructed when disabled");
        }
    }

    #[test]
    fn disabled_message_is_actionable() {
        assert!(disabled_message().contains("ORIGIN_SELFDEV=1"));
    }

    #[test]
    fn approval_authority_grants_only_after_approve_then_consumes() {
        let state = SelfDevState {
            driver: Mutex::new(SelfDevDriver::new(SelfDevConfig::default())),
            approved: AtomicBool::new(false),
        };
        let authority = ApprovalAuthority::new(&state);
        let ctx = ReloadContext::new("j1");

        // No approval ⇒ Deny.
        assert!(matches!(
            authority.authorize(&ctx),
            RestartDecision::Deny(_)
        ));

        // After approve ⇒ Grant, exactly once (the flag is consumed).
        state.approve();
        assert_eq!(authority.authorize(&ctx), RestartDecision::Grant);
        assert!(
            matches!(authority.authorize(&ctx), RestartDecision::Deny(_)),
            "approval must be single-use per job"
        );
    }

    #[test]
    fn shadow_rollback_unscoped_restores_working_tree() {
        // cat-file -e (exists) then checkout . — both succeed.
        let mock = MockGit::scripted(vec![Ok(String::new()), Ok(String::new())]);
        let shadow = ShadowGit::new(&mock, "shadow".to_string());
        let rb = ShadowRollback::new(shadow, "cafe");
        let job = BuildJob::new("j", "x");
        assert_eq!(rb.rollback(&job).unwrap(), RollbackOutcome::Restored);
    }

    #[test]
    fn shadow_rollback_failure_surfaces_as_err() {
        // cat-file -e fails ⇒ NotFound ⇒ rollback Err (operator-escalation).
        let mock = MockGit::scripted(vec![Err(VcsError::Git("missing".into()))]);
        let shadow = ShadowGit::new(&mock, "shadow".to_string());
        let rb = ShadowRollback::new(shadow, "ghost");
        let job = BuildJob::new("j", "x");
        assert!(rb.rollback(&job).is_err());
    }

    #[test]
    fn reload_store_path_is_under_selfdev() {
        let p = reload_store_path();
        assert!(p.ends_with(std::path::Path::new("selfdev").join("reload.json")));
    }
}
