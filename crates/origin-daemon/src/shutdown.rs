// SPDX-License-Identifier: Apache-2.0
//! Phased cooperative shutdown — N8.10 — for `origin-daemon`.
//!
//! Each phase has its own budget timer. A stuck phase force-advances and the
//! driver returns `ShutdownReport::ForcedAdvance(phase)`. The driver is a
//! builder over [`PhaseCallback`]s so the caller — typically `main.rs` —
//! captures the actual subsystem handles (IPC listener stop signal, sidecar,
//! CAS, `SQLite` store, …) and provides one closure per phase. Unset phases
//! are no-ops, so the driver works equally well in tests (just instruments
//! the ordering channel) and in production (runs every wired callback).

#![allow(clippy::module_name_repetitions)]

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::sync::mpsc;

/// Ordered phases the cooperative shutdown driver walks through.
///
/// Order is the N8.10 contract: stop accepting work first, cancel
/// best-effort tasks, drain critical work, then persist state, then close
/// transports, then release shared resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShutdownPhase {
    StopAcceptingIpc,
    CancelBulkAndBackground,
    DrainCritical,
    PersistSidecarQueue,
    FlushCasWriteBuffer,
    CheckpointSqlite,
    CloseIpc,
    ReleaseSharedMemoryAndArenas,
}

const ALL_PHASES: &[ShutdownPhase] = &[
    ShutdownPhase::StopAcceptingIpc,
    ShutdownPhase::CancelBulkAndBackground,
    ShutdownPhase::DrainCritical,
    ShutdownPhase::PersistSidecarQueue,
    ShutdownPhase::FlushCasWriteBuffer,
    ShutdownPhase::CheckpointSqlite,
    ShutdownPhase::CloseIpc,
    ShutdownPhase::ReleaseSharedMemoryAndArenas,
];

/// Outcome of a shutdown run.
#[derive(Debug, Clone, Copy)]
pub enum ShutdownReport {
    /// All phases completed within their per-phase budget.
    Clean,
    /// The named phase exceeded its budget and the driver force-advanced
    /// past it. Any remaining phases are skipped because they would block
    /// on the same hung resource.
    ForcedAdvance(ShutdownPhase),
}

/// Erased per-phase callback. The driver invokes one per phase if installed.
pub type PhaseCallback = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

/// Cooperative phased shutdown driver.
///
/// The production wiring installs per-phase callbacks via [`Self::on`] (see
/// `main.rs`); tests typically use [`Self::for_test`] / [`Self::for_test_with_hang`]
/// to exercise the ordering and budget-timer contract without binding real
/// subsystem state.
pub struct CooperativeShutdown {
    tx: mpsc::UnboundedSender<ShutdownPhase>,
    budget: Duration,
    hang_at: Option<ShutdownPhase>,
    callbacks: [Option<PhaseCallback>; ALL_PHASES.len()],
}

impl CooperativeShutdown {
    /// Build a driver with the given phase channel + per-phase budget and no
    /// callbacks installed. Useful both for tests (the channel surfaces phase
    /// ordering) and as the base for the production builder.
    #[must_use]
    pub fn new(tx: mpsc::UnboundedSender<ShutdownPhase>, budget: Duration) -> Self {
        Self {
            tx,
            budget,
            hang_at: None,
            callbacks: Default::default(),
        }
    }

    /// Install (or replace) a per-phase callback. Returns `self` for builder
    /// chaining. Each callback runs at most once and is consumed by `run`.
    #[must_use]
    pub fn on<F, Fut>(mut self, phase: ShutdownPhase, action: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let slot = phase_index(phase);
        self.callbacks[slot] = Some(Box::new(move || Box::pin(action())));
        self
    }

    /// Test constructor — phases are no-ops that complete instantly.
    #[must_use]
    pub fn for_test(tx: mpsc::UnboundedSender<ShutdownPhase>, budget: Duration) -> Self {
        Self::new(tx, budget)
    }

    /// Test constructor — `hang_at` simulates a phase that never completes,
    /// so the budget timer must force-advance.
    #[must_use]
    pub fn for_test_with_hang(
        tx: mpsc::UnboundedSender<ShutdownPhase>,
        hang_at: ShutdownPhase,
        budget: Duration,
    ) -> Self {
        let mut s = Self::new(tx, budget);
        s.hang_at = Some(hang_at);
        s
    }

    /// Production constructor — returns a driver with the 30s budget the
    /// daemon uses by default and no callbacks installed. Caller (typically
    /// `main.rs`) chains [`Self::on`] to bind real subsystem handles.
    #[must_use]
    pub fn for_production() -> Self {
        // The unbounded receiver is dropped — the sends fail silently, which
        // is the right behaviour outside test instrumentation.
        let (tx, _rx) = mpsc::unbounded_channel::<ShutdownPhase>();
        Self::new(tx, Duration::from_secs(30))
    }

    /// Drive every phase to completion (or force-advance on budget overflow).
    ///
    /// # Errors
    /// The driver is infallible: individual phase callbacks return `()` so
    /// any failure must already be logged inside the callback. The
    /// `anyhow::Result` return is preserved for forward compatibility.
    pub async fn run(&mut self) -> anyhow::Result<ShutdownReport> {
        for phase in ALL_PHASES {
            let _ = self.tx.send(*phase);
            let cb = self.callbacks[phase_index(*phase)].take();
            let hang = self.hang_at == Some(*phase);
            let budget = self.budget;
            let work = run_phase(cb, hang);
            let outcome = tokio::time::timeout(budget, work).await;
            if outcome.is_err() {
                tracing::warn!(?phase, "shutdown: phase exceeded budget — force-advancing");
                return Ok(ShutdownReport::ForcedAdvance(*phase));
            }
        }
        Ok(ShutdownReport::Clean)
    }
}

async fn run_phase(cb: Option<PhaseCallback>, hang: bool) {
    if hang {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        return;
    }
    if let Some(cb) = cb {
        cb().await;
    } else {
        tokio::task::yield_now().await;
    }
}

fn phase_index(p: ShutdownPhase) -> usize {
    ALL_PHASES.iter().position(|q| *q == p).unwrap_or(0)
}
