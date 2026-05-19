//! `PlanHandle` — Arc-wrapped fold + persistence + broadcast (P9.6).
//!
//! The swarm crate is the first consumer of the plan op log, so the handle
//! lives here rather than in `origin-plan` (which owns only the pure CRDT
//! types). `apply` is the single funnel for all op authoring: it
//!
//! 1. extends the in-memory op log,
//! 2. **re-folds** the entire log to produce the new `Plan` snapshot
//!    (deterministic per `origin_plan::fold`),
//! 3. persists the envelope via `PlanStore::append_op`, and
//! 4. broadcasts the envelope to every subscriber.
//!
//! Re-folding on every op is O(n log n) per call. The `origin-plan` fold takes
//! `IntoIterator<Item = OpEnvelope>` and re-sorts internally, so there is no
//! single-op fast path on disk; replaying the log is the canonical way to
//! materialise state. P9.3's snapshot compaction keeps the log bounded, so the
//! amortised cost stays low in practice. If a hot-loop workload ever needs an
//! incremental apply we can add `Plan::apply(&OpEnvelope)` upstream — for now
//! "fold the log" matches the on-disk API exactly.

use std::sync::Arc;

use origin_plan::{fold, OpEnvelope, Plan, PlanStore};
use tokio::sync::{broadcast, Mutex};

use crate::error::SwarmError;

/// Capacity of the per-handle broadcast channel. Subscribers that fall more
/// than `BROADCAST_CAP` messages behind will see `RecvError::Lagged` — they
/// can recover by re-`snapshot()`-ing the plan.
const BROADCAST_CAP: usize = 64;

/// Coordinator-side handle to the shared plan.
///
/// Cheap to clone: every field is already `Arc` / `Sender`-backed. Workers
/// receive a clone via their `WorkerContext`; the TUI panel (P9.9) holds
/// another clone via the daemon.
pub struct PlanHandle {
    inner: Arc<Mutex<Plan>>,
    log: Arc<Mutex<Vec<OpEnvelope>>>,
    store: Arc<PlanStore>,
    broadcast: broadcast::Sender<OpEnvelope>,
}

impl Clone for PlanHandle {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            log: Arc::clone(&self.log),
            store: Arc::clone(&self.store),
            broadcast: self.broadcast.clone(),
        }
    }
}

impl PlanHandle {
    /// Construct a handle around an existing `Plan` + `PlanStore`.
    ///
    /// The caller passes the `Plan` so it can also feed the same `Mutex` into
    /// any pre-existing observer; the handle takes shared ownership.
    #[must_use]
    pub fn new(inner: Arc<Mutex<Plan>>, store: Arc<PlanStore>) -> Self {
        let (broadcast, _) = broadcast::channel(BROADCAST_CAP);
        Self {
            inner,
            log: Arc::new(Mutex::new(Vec::new())),
            store,
            broadcast,
        }
    }

    /// Apply `op`: re-fold + persist + broadcast.
    ///
    /// # Errors
    /// Returns [`SwarmError::Plan`] if the persistence layer fails.
    /// Broadcast send errors are silently ignored — a missing subscriber is
    /// not a fold-failure.
    pub async fn apply(&self, op: OpEnvelope) -> Result<(), SwarmError> {
        // Build the new fold under the log lock, then drop the log guard
        // before re-acquiring the plan lock — keeps the two mutexes from
        // overlapping holds (Clippy `significant_drop_tightening`).
        let new_plan = {
            let mut log = self.log.lock().await;
            log.push(op.clone());
            // Re-fold the entire log into the canonical Plan. Cheap in
            // practice because P9.3 snapshot compaction GCs old ops.
            fold(log.iter().cloned())
        };
        {
            let mut guard = self.inner.lock().await;
            *guard = new_plan;
        }
        self.store.append_op(&op).map_err(SwarmError::Plan)?;
        let _ = self.broadcast.send(op);
        Ok(())
    }

    /// Subscribe to every future op applied via [`Self::apply`].
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<OpEnvelope> {
        self.broadcast.subscribe()
    }

    /// Take a cheap clone of the current `Plan` fold.
    pub async fn snapshot(&self) -> Plan {
        self.inner.lock().await.clone()
    }
}
