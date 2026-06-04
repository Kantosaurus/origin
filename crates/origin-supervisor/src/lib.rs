// SPDX-License-Identifier: Apache-2.0
//! `origin-supervisor` library surface — exposes the resume-token replay
//! helpers so the supervisor binary and integration tests can share them
//! without duplicating the wire format.
//!
//! The `ResumeToken` shape itself lives in the leaf crate
//! `origin-resume-token` so the daemon and supervisor can both depend on
//! it without a dependency cycle.

#![forbid(unsafe_code)]

pub mod ipc_resume;
pub mod lifecycle;
pub mod relaunch;
pub mod resume_token;

pub use lifecycle::{
    AttachState, Decision, HandoffKind, LifecycleConfig, LifecyclePolicy, MemReading, RetireReason,
    SessionClass, SessionState,
};
pub use relaunch::{
    decide_relaunch, default_relaunch_manifest_path, load_manifest, perform_swap, should_rollback,
    watch_outcome, RelaunchDecision, RelaunchManifest, WatchOutcome, SELFDEV_RELAUNCH_EXIT_CODE,
};
