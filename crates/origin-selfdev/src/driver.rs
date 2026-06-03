// SPDX-License-Identifier: Apache-2.0
//! The supervised self-dev orchestration state machine.
//!
//! [`SelfDevDriver`] is a pure state machine: feed it [`SelfDevEvent`]s and it
//! advances [`SelfDevState`], invoking injected effect traits at the moments the
//! safety rules demand. It performs no I/O itself — every dangerous action goes
//! through [`BuildRunner`], [`TestRunner`], [`Rollback`], or
//! [`RestartAuthority`]. That keeps the full state graph unit-testable with
//! fakes and means the daemon decides *how* effects happen while the machine
//! decides *whether* and *in what order* they may.
//!
//! See the crate root for the four safety invariants this machine enforces.

use crate::context::{ReloadContext, ReloadStore};
use crate::queue::{BuildJob, BuildQueue};

/// The orchestration state for a self-dev cycle.
///
/// The happy path is `Idle → Editing → Building → Testing → AwaitingRestart →
/// Resuming → Idle`. Any failure during `Building`/`Testing` routes through
/// `Failed → RolledBack → Idle`, leaving the running binary untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfDevState {
    /// No job in flight. Ready to start the next queued job.
    Idle,
    /// A job is dequeued; source edits are being applied.
    Editing,
    /// Source edited; a build is running.
    Building,
    /// Build succeeded; the test suite is running.
    Testing,
    /// Build AND test succeeded; awaiting an explicit restart grant. The
    /// machine will not leave this state on its own.
    AwaitingRestart,
    /// Restart granted; the successor process is resuming from the persisted
    /// [`ReloadContext`].
    Resuming,
    /// A build or test failed. Carries the human-readable reason. Transient:
    /// the driver immediately rolls back into [`SelfDevState::RolledBack`].
    Failed(String),
    /// Rollback has run; the working tree is restored and the running binary is
    /// unchanged. Returns to [`SelfDevState::Idle`] on the next step.
    RolledBack,
}

impl SelfDevState {
    /// Whether the machine is mid-cycle (a job is in flight).
    #[must_use]
    pub const fn is_busy(&self) -> bool {
        !matches!(self, Self::Idle)
    }
}

/// External events that drive [`SelfDevDriver`].
///
/// `BuildResult`/`TestResult` carry the *outcome* of running the corresponding
/// step. Two ways to drive the machine: feed these events directly via
/// [`SelfDevDriver::handle`] (used in tests and when the daemon runs build/test
/// out-of-band), or call the `run_*` helpers, which invoke the injected runner
/// and fold its result into the matching event for you.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfDevEvent {
    /// Begin the next queued job: dequeue it and enter `Editing`.
    StartJob,
    /// Source edits for the in-flight job are complete; begin building.
    EditDone,
    /// A build finished with the given success flag.
    BuildResult {
        /// `true` if the build succeeded.
        ok: bool,
    },
    /// A test run finished with the given success flag.
    TestResult {
        /// `true` if the tests passed.
        ok: bool,
    },
    /// A restart was explicitly granted (e.g. by an operator/policy gate).
    RestartGranted,
    /// The successor process reported it has resumed from the reload context.
    Resumed,
}

/// What a [`Rollback`] actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollbackOutcome {
    /// The working tree was restored to the pre-edit checkpoint.
    Restored,
    /// Nothing needed restoring (no edits had landed yet).
    NothingToRestore,
}

/// A restart authority's decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartDecision {
    /// Allow the supervised restart now.
    Grant,
    /// Refuse the restart, with a reason (e.g. operator on the loop, or a
    /// safety policy). The machine stays in `AwaitingRestart`.
    Deny(String),
}

/// Runs the project build for a self-dev job. Implementors own all process I/O.
pub trait BuildRunner {
    /// Build the project for `job`. Return `Ok(())` on a clean build, or an
    /// `Err` whose payload is a human-readable failure reason.
    ///
    /// # Errors
    /// Returns the build failure reason when the build does not succeed.
    fn build(&self, job: &BuildJob) -> Result<(), String>;
}

/// Runs the project tests for a self-dev job. Implementors own all process I/O.
pub trait TestRunner {
    /// Test the project for `job`. Return `Ok(())` when the suite passes, or an
    /// `Err` whose payload is a human-readable failure reason.
    ///
    /// # Errors
    /// Returns the test failure reason when the suite does not pass.
    fn test(&self, job: &BuildJob) -> Result<(), String>;
}

/// Restores the working tree after a failed self-dev job (shadow-git restore /
/// `git restore`). Implementors own all VCS/filesystem I/O.
pub trait Rollback {
    /// Restore the tree for the failed `job`. Return what was done, or an `Err`
    /// reason if the rollback itself failed (a serious condition the daemon
    /// must surface — the tree may be dirty).
    ///
    /// # Errors
    /// Returns a reason when the restore could not be completed.
    fn rollback(&self, job: &BuildJob) -> Result<RollbackOutcome, String>;
}

/// The gate that must explicitly grant a supervised restart. The machine never
/// restarts on its own — it asks this authority and obeys.
pub trait RestartAuthority {
    /// Decide whether the supervised restart for `ctx` may proceed now.
    fn authorize(&self, ctx: &ReloadContext) -> RestartDecision;
}

/// Tuning for the safety guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelfDevConfig {
    /// Maximum consecutive *failed* generations before the storm guard trips
    /// and the driver refuses to start further self-dev jobs. Must be >= 1; a
    /// value of 0 is clamped to 1 by [`SelfDevDriver::new`].
    pub max_consecutive_failures: u32,
}

impl Default for SelfDevConfig {
    fn default() -> Self {
        Self {
            max_consecutive_failures: 3,
        }
    }
}

/// Errors returned by [`SelfDevDriver`] when an event cannot be applied.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SelfDevError {
    /// The event is not valid from the current state.
    #[error("invalid transition: {event} from state {state}")]
    InvalidTransition {
        /// The rejected event (debug-formatted).
        event: String,
        /// The state it was rejected from (debug-formatted).
        state: String,
    },
    /// `StartJob` was issued but the queue is empty.
    #[error("no queued job to start")]
    QueueEmpty,
    /// The storm guard has tripped; self-dev is disabled until reset.
    #[error("storm guard tripped after {0} consecutive failures; self-dev disabled")]
    StormGuardTripped(u32),
    /// A restart was requested but the authority denied it.
    #[error("restart denied: {0}")]
    RestartDenied(String),
    /// The injected rollback itself failed — the tree may be dirty.
    #[error("rollback failed: {0}")]
    RollbackFailed(String),
    /// Persisting the [`ReloadContext`] before a granted restart failed; the
    /// restart is aborted and the machine stays in `AwaitingRestart`.
    #[error("reload-context persist failed: {0}")]
    PersistFailed(String),
}

/// The self-dev orchestration driver: a pure state machine over a job queue with
/// injected effects.
///
/// Construct with [`SelfDevDriver::new`], enqueue [`BuildJob`]s, then either:
/// * drive the graph directly with [`SelfDevDriver::handle`] (used in tests and
///   when the daemon runs build/test out-of-band), or
/// * use the run helpers ([`SelfDevDriver::run_build`], etc.) that invoke the
///   injected runners and fold their results into the right event.
#[allow(clippy::module_name_repetitions)] // `SelfDevDriver` is the documented public type re-exported at the crate root.
pub struct SelfDevDriver {
    config: SelfDevConfig,
    state: SelfDevState,
    queue: BuildQueue,
    /// The job currently in flight (dequeued), if any.
    current: Option<BuildJob>,
    /// Consecutive failed generations; reset to 0 on any successful cycle.
    consecutive_failures: u32,
    /// Total generations that have reached a *granted* restart. Mirrors the
    /// [`ReloadContext::generation`] counter the daemon persists.
    generation: u64,
}

impl SelfDevDriver {
    /// Create an idle driver with the given config. A `max_consecutive_failures`
    /// of 0 is clamped to 1 so the storm guard is always meaningful.
    #[must_use]
    pub fn new(mut config: SelfDevConfig) -> Self {
        if config.max_consecutive_failures == 0 {
            config.max_consecutive_failures = 1;
        }
        Self {
            config,
            state: SelfDevState::Idle,
            queue: BuildQueue::new(),
            current: None,
            consecutive_failures: 0,
            generation: 0,
        }
    }

    /// The current state.
    #[must_use]
    pub const fn state(&self) -> &SelfDevState {
        &self.state
    }

    /// The job currently in flight, if any.
    #[must_use]
    pub const fn current(&self) -> Option<&BuildJob> {
        self.current.as_ref()
    }

    /// Consecutive failed generations since the last success.
    #[must_use]
    pub const fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Number of generations that have reached a granted restart.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether the storm guard has tripped (too many consecutive failures).
    #[must_use]
    pub const fn storm_guard_tripped(&self) -> bool {
        self.consecutive_failures >= self.config.max_consecutive_failures
    }

    /// Enqueue a self-modification job (FIFO).
    pub fn enqueue(&mut self, job: BuildJob) {
        self.queue.enqueue(job);
    }

    /// Number of jobs waiting in the queue (excludes the in-flight job).
    #[must_use]
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Manually reset the storm guard, re-enabling self-dev. The daemon calls
    /// this only after an operator has acknowledged the failures.
    pub fn reset_storm_guard(&mut self) {
        self.consecutive_failures = 0;
    }

    /// Advance the machine by one event. This is the single source of truth for
    /// the transition table and all safety invariants; the higher-level
    /// `run_*` helpers funnel through it.
    ///
    /// # Errors
    /// Returns [`SelfDevError`] when the event is invalid from the current
    /// state, the queue is empty on `StartJob`, or the storm guard is tripped.
    pub fn handle(&mut self, event: &SelfDevEvent) -> Result<(), SelfDevError> {
        match (&self.state, event) {
            // --- Start a job: only from Idle, only if not stormed, only if a
            // job is queued. This is the sole entry into a cycle.
            (SelfDevState::Idle, SelfDevEvent::StartJob) => {
                if self.storm_guard_tripped() {
                    return Err(SelfDevError::StormGuardTripped(self.consecutive_failures));
                }
                let job = self.queue.dequeue().ok_or(SelfDevError::QueueEmpty)?;
                self.current = Some(job);
                self.state = SelfDevState::Editing;
                Ok(())
            }

            // --- Edit complete -> build.
            (SelfDevState::Editing, SelfDevEvent::EditDone) => {
                self.state = SelfDevState::Building;
                Ok(())
            }

            // --- Build result.
            (SelfDevState::Building, SelfDevEvent::BuildResult { ok }) => {
                if *ok {
                    self.state = SelfDevState::Testing;
                } else {
                    // SAFETY RULE (a): a failed build can NEVER reach
                    // AwaitingRestart. Route to Failed.
                    self.state = SelfDevState::Failed("build failed".to_string());
                }
                Ok(())
            }

            // --- Test result. Only a passing test that followed a passing
            // build (we are in Testing, which is only reachable from a green
            // build) may reach AwaitingRestart. SAFETY RULE (a).
            (SelfDevState::Testing, SelfDevEvent::TestResult { ok }) => {
                if *ok {
                    self.state = SelfDevState::AwaitingRestart;
                } else {
                    self.state = SelfDevState::Failed("test failed".to_string());
                }
                Ok(())
            }

            // --- Restart granted. SAFETY RULE (c): this edge is only legal
            // from AwaitingRestart, which is only reachable with build+test
            // green. The grant itself comes from outside (the authority);
            // `handle` trusts that the caller obtained it. Use
            // `request_restart` to consult an injected authority.
            (SelfDevState::AwaitingRestart, SelfDevEvent::RestartGranted) => {
                self.generation = self.generation.saturating_add(1);
                self.state = SelfDevState::Resuming;
                Ok(())
            }

            // --- Successor resumed -> cycle complete, back to Idle. A
            // successful cycle clears the failure streak.
            (SelfDevState::Resuming, SelfDevEvent::Resumed) => {
                self.consecutive_failures = 0;
                self.current = None;
                self.state = SelfDevState::Idle;
                Ok(())
            }

            _ => Err(SelfDevError::InvalidTransition {
                event: format!("{event:?}"),
                state: format!("{:?}", self.state),
            }),
        }
    }

    /// Run the injected build for the in-flight job and fold the result into the
    /// state machine. Must be called from `Building`.
    ///
    /// # Errors
    /// Returns [`SelfDevError::InvalidTransition`] if not in `Building`.
    pub fn run_build(&mut self, runner: &dyn BuildRunner) -> Result<bool, SelfDevError> {
        let Some(job) = self.current.clone() else {
            return Err(self.invalid(&SelfDevEvent::BuildResult { ok: false }));
        };
        if self.state != SelfDevState::Building {
            return Err(self.invalid(&SelfDevEvent::BuildResult { ok: false }));
        }
        let ok = runner.build(&job).is_ok();
        self.handle(&SelfDevEvent::BuildResult { ok })?;
        Ok(ok)
    }

    /// Run the injected tests for the in-flight job and fold the result into the
    /// state machine. Must be called from `Testing`.
    ///
    /// # Errors
    /// Returns [`SelfDevError::InvalidTransition`] if not in `Testing`.
    pub fn run_test(&mut self, runner: &dyn TestRunner) -> Result<bool, SelfDevError> {
        let Some(job) = self.current.clone() else {
            return Err(self.invalid(&SelfDevEvent::TestResult { ok: false }));
        };
        if self.state != SelfDevState::Testing {
            return Err(self.invalid(&SelfDevEvent::TestResult { ok: false }));
        }
        let ok = runner.test(&job).is_ok();
        self.handle(&SelfDevEvent::TestResult { ok })?;
        Ok(ok)
    }

    /// Perform the rollback for a `Failed` state via the injected [`Rollback`],
    /// counting the failed generation against the storm guard and returning the
    /// machine to `Idle` on the same binary. SAFETY RULE (b).
    ///
    /// # Errors
    /// Returns [`SelfDevError::InvalidTransition`] if not in `Failed`, or
    /// [`SelfDevError::RollbackFailed`] if the injected rollback errored — in
    /// which case the machine stays in `Failed` so the daemon can intervene.
    pub fn run_rollback(
        &mut self,
        rollback: &dyn Rollback,
    ) -> Result<RollbackOutcome, SelfDevError> {
        let SelfDevState::Failed(_) = &self.state else {
            return Err(self.invalid(&SelfDevEvent::EditDone));
        };
        let job = self.current.clone().unwrap_or_else(|| BuildJob::new("", ""));
        let outcome = rollback
            .rollback(&job)
            .map_err(SelfDevError::RollbackFailed)?;
        // Count this failed generation, then settle to Idle on the SAME binary.
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.current = None;
        self.state = SelfDevState::RolledBack;
        // RolledBack is transient: settle straight to Idle.
        self.state = SelfDevState::Idle;
        Ok(outcome)
    }

    /// Consult the injected [`RestartAuthority`] from `AwaitingRestart`. On a
    /// grant, persist `ctx` via `store`, advance to `Resuming`, and return
    /// `Ok(())`. On a deny, stay in `AwaitingRestart` and return
    /// [`SelfDevError::RestartDenied`]. SAFETY RULE (c).
    ///
    /// # Errors
    /// Returns [`SelfDevError::InvalidTransition`] if not in `AwaitingRestart`,
    /// [`SelfDevError::RestartDenied`] on a deny, or
    /// [`SelfDevError::PersistFailed`] if persisting `ctx` fails — in either
    /// failure case the machine stays in `AwaitingRestart`.
    pub fn request_restart(
        &mut self,
        authority: &dyn RestartAuthority,
        store: &dyn ReloadStore,
        ctx: &ReloadContext,
    ) -> Result<(), SelfDevError> {
        if self.state != SelfDevState::AwaitingRestart {
            return Err(self.invalid(&SelfDevEvent::RestartGranted));
        }
        match authority.authorize(ctx) {
            RestartDecision::Grant => {
                // Persist BEFORE flipping state: if the save fails we must not
                // claim a restart is happening.
                store
                    .save(ctx)
                    .map_err(|e| SelfDevError::PersistFailed(e.to_string()))?;
                self.handle(&SelfDevEvent::RestartGranted)
            }
            RestartDecision::Deny(reason) => Err(SelfDevError::RestartDenied(reason)),
        }
    }

    fn invalid(&self, event: &SelfDevEvent) -> SelfDevError {
        SelfDevError::InvalidTransition {
            event: format!("{event:?}"),
            state: format!("{:?}", self.state),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct OkBuild;
    impl BuildRunner for OkBuild {
        fn build(&self, _job: &BuildJob) -> Result<(), String> {
            Ok(())
        }
    }
    struct FailBuild;
    impl BuildRunner for FailBuild {
        fn build(&self, _job: &BuildJob) -> Result<(), String> {
            Err("compile error".into())
        }
    }
    struct OkTest;
    impl TestRunner for OkTest {
        fn test(&self, _job: &BuildJob) -> Result<(), String> {
            Ok(())
        }
    }
    struct FailTest;
    impl TestRunner for FailTest {
        fn test(&self, _job: &BuildJob) -> Result<(), String> {
            Err("assertion failed".into())
        }
    }

    #[derive(Default)]
    struct RecordingRollback {
        calls: Cell<u32>,
    }
    impl Rollback for RecordingRollback {
        fn rollback(&self, _job: &BuildJob) -> Result<RollbackOutcome, String> {
            self.calls.set(self.calls.get() + 1);
            Ok(RollbackOutcome::Restored)
        }
    }
    struct FailingRollback;
    impl Rollback for FailingRollback {
        fn rollback(&self, _job: &BuildJob) -> Result<RollbackOutcome, String> {
            Err("git restore failed".into())
        }
    }

    struct Granting;
    impl RestartAuthority for Granting {
        fn authorize(&self, _ctx: &ReloadContext) -> RestartDecision {
            RestartDecision::Grant
        }
    }
    struct Denying;
    impl RestartAuthority for Denying {
        fn authorize(&self, _ctx: &ReloadContext) -> RestartDecision {
            RestartDecision::Deny("operator in the loop".into())
        }
    }

    #[derive(Default)]
    struct FakeStore {
        slot: std::cell::RefCell<Option<ReloadContext>>,
    }
    impl ReloadStore for FakeStore {
        fn save(&self, ctx: &ReloadContext) -> Result<(), crate::context::StoreError> {
            *self.slot.borrow_mut() = Some(ctx.clone());
            Ok(())
        }
        fn load(&self) -> Result<Option<ReloadContext>, crate::context::StoreError> {
            Ok(self.slot.borrow().clone())
        }
        fn clear(&self) -> Result<(), crate::context::StoreError> {
            *self.slot.borrow_mut() = None;
            Ok(())
        }
    }

    fn driver_with_one_job() -> SelfDevDriver {
        let mut d = SelfDevDriver::new(SelfDevConfig::default());
        d.enqueue(BuildJob::new("j1", "speed up the planner"));
        d
    }

    #[test]
    fn happy_path_reaches_awaiting_restart_only_when_both_green() {
        let mut d = driver_with_one_job();
        assert_eq!(d.state(), &SelfDevState::Idle);
        d.handle(&SelfDevEvent::StartJob).unwrap();
        assert_eq!(d.state(), &SelfDevState::Editing);
        assert_eq!(d.current().unwrap().id, "j1");
        d.handle(&SelfDevEvent::EditDone).unwrap();
        assert_eq!(d.state(), &SelfDevState::Building);
        d.run_build(&OkBuild).unwrap();
        assert_eq!(d.state(), &SelfDevState::Testing);
        d.run_test(&OkTest).unwrap();
        assert_eq!(d.state(), &SelfDevState::AwaitingRestart);
    }

    #[test]
    fn full_happy_path_returns_to_idle_after_resume() {
        let mut d = driver_with_one_job();
        d.handle(&SelfDevEvent::StartJob).unwrap();
        d.handle(&SelfDevEvent::EditDone).unwrap();
        d.handle(&SelfDevEvent::BuildResult { ok: true }).unwrap();
        d.handle(&SelfDevEvent::TestResult { ok: true }).unwrap();
        d.handle(&SelfDevEvent::RestartGranted).unwrap();
        assert_eq!(d.state(), &SelfDevState::Resuming);
        assert_eq!(d.generation(), 1);
        d.handle(&SelfDevEvent::Resumed).unwrap();
        assert_eq!(d.state(), &SelfDevState::Idle);
        assert!(d.current().is_none());
        assert_eq!(d.consecutive_failures(), 0);
    }

    #[test]
    fn failed_build_can_never_reach_awaiting_restart() {
        let mut d = driver_with_one_job();
        d.handle(&SelfDevEvent::StartJob).unwrap();
        d.handle(&SelfDevEvent::EditDone).unwrap();
        let ok = d.run_build(&FailBuild).unwrap();
        assert!(!ok);
        assert_eq!(d.state(), &SelfDevState::Failed("build failed".into()));
        // There is no edge from Failed to AwaitingRestart.
        assert!(d.handle(&SelfDevEvent::RestartGranted).is_err());
    }

    #[test]
    fn failed_build_rolls_back_to_idle_same_binary() {
        let mut d = driver_with_one_job();
        d.handle(&SelfDevEvent::StartJob).unwrap();
        d.handle(&SelfDevEvent::EditDone).unwrap();
        d.run_build(&FailBuild).unwrap();
        let rb = RecordingRollback::default();
        let outcome = d.run_rollback(&rb).unwrap();
        assert_eq!(outcome, RollbackOutcome::Restored);
        assert_eq!(rb.calls.get(), 1);
        assert_eq!(d.state(), &SelfDevState::Idle);
        // No generation was granted; the binary never changed.
        assert_eq!(d.generation(), 0);
        assert_eq!(d.consecutive_failures(), 1);
    }

    #[test]
    fn failed_test_rolls_back() {
        let mut d = driver_with_one_job();
        d.handle(&SelfDevEvent::StartJob).unwrap();
        d.handle(&SelfDevEvent::EditDone).unwrap();
        d.run_build(&OkBuild).unwrap();
        let ok = d.run_test(&FailTest).unwrap();
        assert!(!ok);
        assert_eq!(d.state(), &SelfDevState::Failed("test failed".into()));
        let rb = RecordingRollback::default();
        d.run_rollback(&rb).unwrap();
        assert_eq!(d.state(), &SelfDevState::Idle);
        assert_eq!(d.consecutive_failures(), 1);
    }

    #[test]
    fn rollback_failure_keeps_machine_in_failed() {
        let mut d = driver_with_one_job();
        d.handle(&SelfDevEvent::StartJob).unwrap();
        d.handle(&SelfDevEvent::EditDone).unwrap();
        d.run_build(&FailBuild).unwrap();
        let err = d.run_rollback(&FailingRollback).unwrap_err();
        assert!(matches!(err, SelfDevError::RollbackFailed(_)));
        // Still Failed so the daemon can escalate; streak not yet counted.
        assert!(matches!(d.state(), SelfDevState::Failed(_)));
        assert_eq!(d.consecutive_failures(), 0);
    }

    #[test]
    fn restart_requires_authority_grant() {
        let mut d = driver_with_one_job();
        d.handle(&SelfDevEvent::StartJob).unwrap();
        d.handle(&SelfDevEvent::EditDone).unwrap();
        d.run_build(&OkBuild).unwrap();
        d.run_test(&OkTest).unwrap();
        assert_eq!(d.state(), &SelfDevState::AwaitingRestart);

        let store = FakeStore::default();
        let ctx = ReloadContext::new("j1");
        // Denied: stays in AwaitingRestart, nothing persisted.
        let err = d.request_restart(&Denying, &store, &ctx).unwrap_err();
        assert!(matches!(err, SelfDevError::RestartDenied(_)));
        assert_eq!(d.state(), &SelfDevState::AwaitingRestart);
        assert!(store.load().unwrap().is_none());

        // Granted: persists and advances.
        d.request_restart(&Granting, &store, &ctx).unwrap();
        assert_eq!(d.state(), &SelfDevState::Resuming);
        assert_eq!(store.load().unwrap().unwrap().in_flight_job_id, "j1");
        assert_eq!(d.generation(), 1);
    }

    #[test]
    fn request_restart_rejected_outside_awaiting() {
        let mut d = driver_with_one_job();
        let store = FakeStore::default();
        let ctx = ReloadContext::new("j1");
        let err = d.request_restart(&Granting, &store, &ctx).unwrap_err();
        assert!(matches!(err, SelfDevError::InvalidTransition { .. }));
    }

    #[test]
    fn storm_guard_trips_after_n_failures() {
        let mut d = SelfDevDriver::new(SelfDevConfig {
            max_consecutive_failures: 2,
        });
        // Two failing cycles.
        for i in 0..2 {
            d.enqueue(BuildJob::new(format!("j{i}"), "x"));
            d.handle(&SelfDevEvent::StartJob).unwrap();
            d.handle(&SelfDevEvent::EditDone).unwrap();
            d.run_build(&FailBuild).unwrap();
            d.run_rollback(&RecordingRollback::default()).unwrap();
        }
        assert_eq!(d.consecutive_failures(), 2);
        assert!(d.storm_guard_tripped());

        // A third attempt is refused at StartJob.
        d.enqueue(BuildJob::new("j2", "x"));
        let err = d.handle(&SelfDevEvent::StartJob).unwrap_err();
        assert_eq!(err, SelfDevError::StormGuardTripped(2));
        assert_eq!(d.state(), &SelfDevState::Idle);
    }

    #[test]
    fn storm_guard_resets_after_a_success() {
        let mut d = SelfDevDriver::new(SelfDevConfig {
            max_consecutive_failures: 3,
        });
        // One failure.
        d.enqueue(BuildJob::new("f", "x"));
        d.handle(&SelfDevEvent::StartJob).unwrap();
        d.handle(&SelfDevEvent::EditDone).unwrap();
        d.run_build(&FailBuild).unwrap();
        d.run_rollback(&RecordingRollback::default()).unwrap();
        assert_eq!(d.consecutive_failures(), 1);

        // Then a full success clears the streak.
        d.enqueue(BuildJob::new("ok", "x"));
        d.handle(&SelfDevEvent::StartJob).unwrap();
        d.handle(&SelfDevEvent::EditDone).unwrap();
        d.run_build(&OkBuild).unwrap();
        d.run_test(&OkTest).unwrap();
        d.handle(&SelfDevEvent::RestartGranted).unwrap();
        d.handle(&SelfDevEvent::Resumed).unwrap();
        assert_eq!(d.consecutive_failures(), 0);
        assert!(!d.storm_guard_tripped());
    }

    #[test]
    fn manual_reset_reenables_after_storm() {
        let mut d = SelfDevDriver::new(SelfDevConfig {
            max_consecutive_failures: 1,
        });
        d.enqueue(BuildJob::new("f", "x"));
        d.handle(&SelfDevEvent::StartJob).unwrap();
        d.handle(&SelfDevEvent::EditDone).unwrap();
        d.run_build(&FailBuild).unwrap();
        d.run_rollback(&RecordingRollback::default()).unwrap();
        assert!(d.storm_guard_tripped());
        d.reset_storm_guard();
        assert!(!d.storm_guard_tripped());
        // Now StartJob is accepted again.
        d.enqueue(BuildJob::new("g", "x"));
        d.handle(&SelfDevEvent::StartJob).unwrap();
        assert_eq!(d.state(), &SelfDevState::Editing);
    }

    #[test]
    fn start_job_on_empty_queue_errors() {
        let mut d = SelfDevDriver::new(SelfDevConfig::default());
        assert_eq!(d.handle(&SelfDevEvent::StartJob).unwrap_err(), SelfDevError::QueueEmpty);
    }

    #[test]
    fn config_zero_failures_is_clamped_to_one() {
        let d = SelfDevDriver::new(SelfDevConfig {
            max_consecutive_failures: 0,
        });
        assert_eq!(d.config.max_consecutive_failures, 1);
    }

    #[test]
    fn out_of_order_events_are_rejected() {
        let mut d = driver_with_one_job();
        // BuildResult before EditDone is invalid.
        assert!(d.handle(&SelfDevEvent::StartJob).is_ok());
        assert!(d.handle(&SelfDevEvent::BuildResult { ok: true }).is_err());
        // TestResult while Editing is invalid.
        assert!(d.handle(&SelfDevEvent::TestResult { ok: true }).is_err());
        // Resumed while Editing is invalid.
        assert!(d.handle(&SelfDevEvent::Resumed).is_err());
    }

    #[test]
    fn run_build_outside_building_is_rejected() {
        let mut d = driver_with_one_job();
        // From Idle, no current job + wrong state.
        assert!(d.run_build(&OkBuild).is_err());
        d.handle(&SelfDevEvent::StartJob).unwrap();
        // From Editing (current set, but wrong state).
        assert!(d.run_build(&OkBuild).is_err());
    }

    #[test]
    fn run_rollback_outside_failed_is_rejected() {
        let mut d = driver_with_one_job();
        assert!(d.run_rollback(&RecordingRollback::default()).is_err());
        d.handle(&SelfDevEvent::StartJob).unwrap();
        assert!(d.run_rollback(&RecordingRollback::default()).is_err());
    }

    #[test]
    fn second_queued_job_runs_after_first_completes() {
        let mut d = SelfDevDriver::new(SelfDevConfig::default());
        d.enqueue(BuildJob::new("a", "1"));
        d.enqueue(BuildJob::new("b", "2"));
        assert_eq!(d.queued(), 2);
        // Run job a to completion.
        d.handle(&SelfDevEvent::StartJob).unwrap();
        assert_eq!(d.current().unwrap().id, "a");
        assert_eq!(d.queued(), 1);
        d.handle(&SelfDevEvent::EditDone).unwrap();
        d.run_build(&OkBuild).unwrap();
        d.run_test(&OkTest).unwrap();
        d.handle(&SelfDevEvent::RestartGranted).unwrap();
        d.handle(&SelfDevEvent::Resumed).unwrap();
        // Now b can start.
        d.handle(&SelfDevEvent::StartJob).unwrap();
        assert_eq!(d.current().unwrap().id, "b");
    }
}
