//! Per-worker `Lifecycle` state plus a `tokio::sync::watch` shim (P9.6).
//!
//! The coordinator publishes lifecycle transitions through the `Sender`; any
//! observer (notably `Coordinator::await_completion`) holds a `Receiver` and
//! awaits the terminal `Done` / `Failed` state.

use serde::{Deserialize, Serialize};

/// Per-worker observable state.
///
/// Transitions are monotonic: `Spawning` → `Running` → (`Reporting` → `Done`)
/// or `Failed`. A worker never returns to an earlier state. `Failed.reason` is
/// surfaced through `SwarmError::Worker` by `await_completion`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lifecycle {
    /// Coordinator has accepted the spec but the worker task isn't running yet.
    Spawning,
    /// Worker task is actively executing.
    Running,
    /// Worker is wrapping up and producing its `CompletionReport`.
    Reporting,
    /// Worker finished cleanly — the matching `CompletionReport` is available
    /// to `await_completion`.
    Done,
    /// Worker exited via an error path.
    Failed {
        /// Stable error description (cloned into `SwarmError::Worker`).
        reason: String,
    },
}

impl Lifecycle {
    /// `true` iff this is a terminal state (`Done` or `Failed`).
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Failed { .. })
    }
}
