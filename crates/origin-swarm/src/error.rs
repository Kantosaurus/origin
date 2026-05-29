// SPDX-License-Identifier: Apache-2.0
//! `SwarmError` — coordinator/worker error surface (P9.6).
//!
//! Wraps the lower-level error types so call sites can `?` through `apply`,
//! ring sends, and worker exits with a single error enum.

use thiserror::Error;

/// Errors surfaced by `Coordinator`, `PlanHandle::apply`, and worker futures.
#[derive(Debug, Error)]
pub enum SwarmError {
    /// Plan persistence error from `origin-plan`.
    #[error("plan: {0}")]
    Plan(origin_plan::PlanStoreError),
    /// SMR ring error from `origin-smr`.
    #[error("smr: {0:?}")]
    Smr(origin_smr::RingError),
    /// Worker exited with an error message.
    #[error("worker: {0}")]
    Worker(String),
    /// Lifecycle state-machine violation (e.g. observer dropped before a
    /// terminal state was published).
    #[error("lifecycle: {0}")]
    Lifecycle(String),
    /// Wall-clock deadline elapsed before completion.
    #[error("timeout")]
    Timeout,
}
