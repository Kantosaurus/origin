//! Daemon-wide registry of pending memory proposals.
//!
//! [`StreamEvent::MemoryProposed`](crate::protocol::StreamEvent::MemoryProposed)
//! events surface candidates to the CLI keyed by a u32 `proposal_id`. When the
//! user later sends a [`ClientMessage::MemoryDecision`] the daemon must
//! recover the proposal's `(body, tags)` so `Accept` can persist it via the
//! [`MemoryHandle`](origin_tools::dispatch::MemoryHandle) path. The decision
//! arrives on a fresh per-connection scope where the originating
//! [`Session`](crate::session::Session) is no longer in memory — hence a
//! process-wide registry rather than per-session state.
//!
//! Allocation strategy: the registry maintains a monotonic `u32` counter so
//! ids are unique across sessions and across concurrent prompt requests. The
//! [`Proposer`](origin_mem::Proposer) still mutates its caller-supplied
//! counter (so dedup-rollback works), but we load a contiguous range from
//! the registry before each scan and write the consumed range back after.
//! Concurrent scans serialize on the registry mutex while the range is
//! allocated; the actual regex work runs without the lock.

use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct PendingProposal {
    pub body: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Default)]
pub struct ProposalRegistry {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    next_id: u32,
    pending: HashMap<u32, PendingProposal>,
}

impl ProposalRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                next_id: 1,
                pending: HashMap::new(),
            }),
        }
    }

    /// Load the current counter value so `Proposer::scan` can run with a
    /// caller-mutable id. Returns the value the next allocation would take.
    ///
    /// # Panics
    /// Panics if the registry mutex is poisoned (a prior caller panicked
    /// while holding it).
    #[allow(clippy::expect_used)]
    pub fn current_id(&self) -> u32 {
        self.inner.lock().expect("proposal_registry mutex").next_id
    }

    /// Persist the new counter value after `Proposer::scan` returned. The
    /// caller passes the post-scan counter so any duplicate-rollback the
    /// proposer performed is reflected.
    ///
    /// # Panics
    /// Panics if the registry mutex is poisoned.
    #[allow(clippy::expect_used)]
    pub fn advance_to(&self, new_next_id: u32) {
        let mut g = self.inner.lock().expect("proposal_registry mutex");
        if new_next_id > g.next_id {
            g.next_id = new_next_id;
        }
    }

    /// Run `f` with exclusive access to the id counter, advancing it by however
    /// many ids `f` consumes. The registry mutex is held for the whole call so
    /// the read → scan → advance sequence is atomic: concurrent prompt scans can
    /// no longer start from the same base id and mint colliding proposal ids.
    /// `f` must be synchronous and must not re-enter the registry (it does not).
    ///
    /// # Panics
    /// Panics if the registry mutex is poisoned.
    #[allow(clippy::expect_used)]
    pub fn with_id_counter<R>(&self, f: impl FnOnce(&mut u32) -> R) -> R {
        let mut g = self.inner.lock().expect("proposal_registry mutex");
        let mut id = g.next_id;
        let r = f(&mut id);
        if id > g.next_id {
            g.next_id = id;
        }
        r
    }

    /// Record a pending proposal so a future `Accept` can recover the
    /// `(body, tags)`. Overwrites any existing entry for the same id (last
    /// emitted wins; ids are monotonic so this only matters under explicit
    /// counter-reset, which the registry does not support).
    ///
    /// # Panics
    /// Panics if the registry mutex is poisoned.
    #[allow(clippy::expect_used)]
    pub fn record(&self, proposal_id: u32, body: String, tags: Vec<String>) {
        let mut g = self.inner.lock().expect("proposal_registry mutex");
        g.pending.insert(proposal_id, PendingProposal { body, tags });
    }

    /// Look up and remove the pending proposal for `proposal_id`. Returns
    /// `None` if the proposal was never recorded, already accepted, or
    /// already rejected.
    ///
    /// # Panics
    /// Panics if the registry mutex is poisoned.
    #[allow(clippy::expect_used)]
    pub fn take(&self, proposal_id: u32) -> Option<PendingProposal> {
        let mut g = self.inner.lock().expect("proposal_registry mutex");
        g.pending.remove(&proposal_id)
    }

    /// Discard a pending proposal without retrieving it. Used by
    /// `Reject` so the entry doesn't linger in the registry.
    ///
    /// # Panics
    /// Panics if the registry mutex is poisoned.
    pub fn drop(&self, proposal_id: u32) {
        let _ = self.take(proposal_id);
    }

    /// Test helper: how many proposals are currently pending.
    #[doc(hidden)]
    #[must_use]
    #[allow(clippy::expect_used)]
    pub fn pending_count(&self) -> usize {
        self.inner.lock().expect("proposal_registry mutex").pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_advances_monotonically() {
        let r = ProposalRegistry::new();
        assert_eq!(r.current_id(), 1);
        r.advance_to(7);
        assert_eq!(r.current_id(), 7);
        // advance_to does not regress
        r.advance_to(3);
        assert_eq!(r.current_id(), 7);
    }

    #[test]
    fn record_and_take_round_trips() {
        let r = ProposalRegistry::new();
        r.record(1, "hi".into(), vec!["t".into()]);
        let p = r.take(1).expect("present");
        assert_eq!(p.body, "hi");
        assert_eq!(p.tags, vec!["t".to_string()]);
        assert!(r.take(1).is_none());
    }

    #[test]
    fn drop_removes_without_returning() {
        let r = ProposalRegistry::new();
        r.record(5, "x".into(), vec![]);
        assert_eq!(r.pending_count(), 1);
        r.drop(5);
        assert_eq!(r.pending_count(), 0);
    }
}
