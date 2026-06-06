// SPDX-License-Identifier: Apache-2.0
//! Binary self-development with supervised hot-reload — safe by construction.
//!
//! `origin` can propose and apply modifications to *its own* source, rebuild,
//! and restart into the new binary while preserving live sessions. That is the
//! highest-risk capability in the project: an unsupervised edit/build/restart
//! loop can brick the running agent or thrash forever. This crate makes the
//! whole orchestration a **pure, fully unit-tested state machine** with every
//! dangerous side effect (source edit, `cargo build`, `cargo test`, process
//! restart, checkpoint/rollback) hidden behind an injected trait. Nothing in
//! here rebuilds, restarts, or writes the user's tree on its own — the daemon
//! supplies the implementations, and tests supply fakes. The machine itself is
//! deterministic and offline.
//!
//! # Why a state machine, not a script
//!
//! Peers (and naive self-modifying agents) drive self-edit as an ad-hoc script:
//! edit, build, "looks fine", restart. The novel mechanism here is to encode
//! the *safety invariants* directly in the type-level transition table so they
//! cannot be skipped:
//!
//! 1. **No restart without green build AND green test.** [`SelfDevState`] only
//!    reaches [`SelfDevState::AwaitingRestart`] from
//!    [`SelfDevState::Testing`] on a passing test that followed a passing
//!    build. There is no other inbound edge.
//! 2. **Any build/test failure rolls back to the same binary.** A failure routes
//!    through [`SelfDevState::Failed`] → [`SelfDevState::RolledBack`] → back to
//!    [`SelfDevState::Idle`], invoking the injected [`Rollback`] (shadow-git /
//!    `git restore`) so the running binary is never left on broken source.
//! 3. **Restart is explicitly granted, never automatic.** The transition out of
//!    `AwaitingRestart` is gated by an injected [`RestartAuthority`]; the machine
//!    will sit in `AwaitingRestart` indefinitely until something *outside* it
//!    grants the restart.
//! 4. **Storm guard.** Consecutive failed generations are counted; once the cap
//!    is hit the machine refuses to start further self-dev jobs until reset.
//!
//! # Layout
//!
//! * [`ReloadContext`] / [`ReloadStore`] — everything needed to resume after a
//!   supervised restart, persisted behind a store trait.
//! * [`BuildJob`] / [`BuildQueue`] — FIFO queue of self-modification jobs, one
//!   in flight at a time.
//! * [`SelfDevDriver`] — the state machine plus its injected effect traits
//!   ([`BuildRunner`], [`TestRunner`], [`Rollback`], [`RestartAuthority`]).
//! * [`cargo`] — a *real* default [`BuildRunner`]/[`TestRunner`] that shells
//!   `cargo build` / `cargo test`. Clearly named and opt-in; Phase 2 chooses it,
//!   tests never touch it.
//! * [`RelaunchManifest`] / [`RelaunchRequest`] / [`RelaunchStore`] — the
//!   binary-swap contract the daemon writes after a granted restart so the
//!   *supervisor* (which does NOT depend on this crate) can exec the new binary.
//!   This is a separate slot from the reload context's `reload.json`.
//!
//! ```
//! use origin_selfdev::{
//!     BuildJob, SelfDevConfig, SelfDevDriver, SelfDevEvent, SelfDevState,
//! };
//!
//! let mut driver = SelfDevDriver::new(SelfDevConfig::default());
//! driver.enqueue(BuildJob::new("job-1", "tighten the planner cache"));
//! // Start the queued job, run the edit, then a green build + green test.
//! driver.handle(&SelfDevEvent::StartJob).unwrap();
//! driver.handle(&SelfDevEvent::EditDone).unwrap();
//! driver.handle(&SelfDevEvent::BuildResult { ok: true }).unwrap();
//! driver.handle(&SelfDevEvent::TestResult { ok: true }).unwrap();
//! // Only now — both green — is a restart even offered.
//! assert_eq!(driver.state(), &SelfDevState::AwaitingRestart);
//! ```

#![forbid(unsafe_code)]

pub mod cargo;
mod context;
mod driver;
mod queue;
mod relaunch;

pub use cargo::CargoRunner;
pub use context::{FileReloadStore, ReloadContext, ReloadStore, StoreError};
pub use driver::{
    BuildRunner, RestartAuthority, RestartDecision, Rollback, RollbackOutcome, SelfDevConfig, SelfDevDriver,
    SelfDevError, SelfDevEvent, SelfDevState, TestRunner,
};
pub use queue::{BuildJob, BuildQueue};
pub use relaunch::{
    FileRelaunchStore, RelaunchManifest, RelaunchRequest, RelaunchStore, SELFDEV_RELAUNCH_EXIT_CODE,
};
