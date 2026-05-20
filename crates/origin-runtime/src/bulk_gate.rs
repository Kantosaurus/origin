//! Bulk-gate — fairness watcher that wakes parked Bulk tasks whenever the
//! Critical semaphore drops to zero permits in flight.

use crate::registry::{critical_in_flight, registry};
use std::sync::Arc;
use tokio::sync::Notify;

/// Test-visible handle on the global bulk gate.
#[must_use]
pub struct BulkGate(Arc<Notify>);

impl BulkGate {
    pub fn current() -> Self {
        Self(Arc::clone(&registry().bulk_gate))
    }

    pub async fn wait_until_idle(&self) {
        // Construct the `Notified` future BEFORE the in-flight check so we are
        // already registered as a waiter when the producer side calls
        // `notify_waiters()`. Otherwise a notification fired between the check
        // and `.await` would be lost (Notify only buffers a single permit, and
        // `notify_waiters` does not buffer at all).
        loop {
            let notified = self.0.notified();
            tokio::pin!(notified);
            if critical_in_flight() == 0 {
                return;
            }
            notified.as_mut().await;
        }
    }

    pub fn notify_all(&self) {
        self.0.notify_waiters();
    }
}
