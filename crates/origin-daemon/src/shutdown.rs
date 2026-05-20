//! Phased cooperative shutdown — N8.10 — for `origin-daemon`.
//!
//! Each phase has its own budget timer. A stuck phase force-advances and the
//! driver returns `ShutdownReport::ForcedAdvance(phase)`. The full set of
//! per-phase callbacks (IPC listener stop, sidecar queue persist, CAS flush,
//! `SQLite` checkpoint, …) is filled in by P14 polish; P12 ships the phase
//! structure + budget timer + ordering contract.

#![allow(clippy::module_name_repetitions)]

use std::time::Duration;
use tokio::sync::mpsc;

/// Ordered phases the cooperative shutdown driver walks through.
///
/// Order is the N8.10 contract: stop accepting work first, cancel
/// best-effort tasks, drain critical work, then persist state, then close
/// transports, then release shared resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Cooperative phased shutdown driver.
///
/// Per-phase callbacks for the production wiring (IPC listener handle,
/// sidecar queue persister, CAS flush hook, `SQLite` checkpoint, …) are a
/// P14 polish item; the test constructors produce a no-op driver that
/// exercises the phase ordering + budget timer contract.
pub struct CooperativeShutdown {
    tx: mpsc::UnboundedSender<ShutdownPhase>,
    budget: Duration,
    hang_at: Option<ShutdownPhase>,
}

impl CooperativeShutdown {
    /// Test constructor — phases are no-ops that complete instantly.
    #[must_use]
    pub const fn for_test(tx: mpsc::UnboundedSender<ShutdownPhase>, budget: Duration) -> Self {
        Self {
            tx,
            budget,
            hang_at: None,
        }
    }

    /// Test constructor — `hang_at` simulates a phase that never completes,
    /// so the budget timer must force-advance.
    #[must_use]
    pub const fn for_test_with_hang(
        tx: mpsc::UnboundedSender<ShutdownPhase>,
        hang_at: ShutdownPhase,
        budget: Duration,
    ) -> Self {
        Self {
            tx,
            budget,
            hang_at: Some(hang_at),
        }
    }

    /// Production constructor — STUB. P12 ships the phase structure +
    /// budget timer; the actual per-phase callbacks (stop accepting IPC,
    /// persist sidecar queue, flush CAS, checkpoint `SQLite`, …) are a P14
    /// polish item.
    ///
    /// The stub is equivalent to `for_test(no_op_channel, 30s_budget)` so
    /// production callers get the right phase ordering without yet
    /// touching real resources.
    #[must_use]
    pub fn for_production() -> Self {
        // The unbounded receiver is intentionally dropped — `run()` will
        // call `tx.send(...)` and the sends will fail silently after this
        // function returns, which is the correct behaviour for the stub.
        let (tx, _rx) = mpsc::unbounded_channel::<ShutdownPhase>();
        Self {
            tx,
            budget: Duration::from_secs(30),
            hang_at: None,
        }
    }

    /// Drive every phase to completion (or force-advance on budget overflow).
    ///
    /// # Errors
    /// The current driver is infallible. The `anyhow::Result` return is
    /// reserved for the P14 polish wiring, where individual phase callbacks
    /// may surface errors that should be logged but not block the shutdown.
    pub async fn run(&mut self) -> anyhow::Result<ShutdownReport> {
        for phase in ALL_PHASES {
            let _ = self.tx.send(*phase);
            let work = self.run_phase(*phase);
            let outcome = tokio::time::timeout(self.budget, work).await;
            if outcome.is_err() {
                tracing::warn!(?phase, "shutdown: phase exceeded budget — force-advancing");
                return Ok(ShutdownReport::ForcedAdvance(*phase));
            }
        }
        Ok(ShutdownReport::Clean)
    }

    async fn run_phase(&self, phase: ShutdownPhase) {
        if self.hang_at == Some(phase) {
            // Sleep past every reasonable budget — caller's `timeout` will fire.
            tokio::time::sleep(Duration::from_secs(3600)).await;
            return;
        }
        // Real wiring lives in P14 polish; the test-mode driver is a no-op.
        tokio::task::yield_now().await;
    }
}
