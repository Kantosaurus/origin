//! Daemon-wide plan-op broadcast bus (P9.9 → P10 follow-up).
//!
//! The swarm `PlanHandle` already exposes a per-handle `tokio::sync::broadcast`
//! channel — but each `PlanHandle` is owned by a `Coordinator`, and the daemon
//! may host multiple coordinators across its lifetime. `PlanBus` is the
//! single fan-out point IPC clients subscribe to. The IPC handler turns
//! subscriber receivers into a stream of [`StreamEvent::PlanOp`] frames.
//!
//! Today nothing inside the daemon's hot path publishes to the bus — it is
//! threaded through `main.rs` so swarm-spawning call sites (and future P10
//! plumbing that merges per-coordinator broadcasts into this one) can call
//! `bus.publish(envelope)`. The wire shape + subscribe path are observable
//! end-to-end via the `tests/plan_bus.rs` integration test.

use std::sync::Arc;

use origin_plan::OpEnvelope;
use tokio::sync::broadcast;

/// Broadcast fan-out capacity. Same constant as
/// [`origin_swarm::PlanHandle`]'s `BROADCAST_CAP` — subscribers that fall
/// further behind see `RecvError::Lagged` and must re-snapshot.
const BROADCAST_CAP: usize = 64;

/// Daemon-wide plan-op broadcast. Cheap to clone (one `Arc<Sender>`).
#[derive(Clone)]
pub struct PlanBus {
    tx: Arc<broadcast::Sender<OpEnvelope>>,
}

impl PlanBus {
    /// Create a fresh bus with no subscribers.
    #[must_use]
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(BROADCAST_CAP);
        Self { tx: Arc::new(tx) }
    }

    /// Broadcast `envelope` to every current subscriber. A bus with no
    /// subscribers silently drops the value — that matches the swarm
    /// `PlanHandle::apply` contract.
    pub fn publish(&self, envelope: OpEnvelope) {
        let _ = self.tx.send(envelope);
    }

    /// Subscribe to the stream. The receiver sees every envelope published
    /// AFTER `subscribe` returns; pre-subscribe envelopes are not buffered.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<OpEnvelope> {
        self.tx.subscribe()
    }
}

impl Default for PlanBus {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PlanBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanBus")
            .field("subscribers", &self.tx.receiver_count())
            .finish()
    }
}
