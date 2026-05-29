// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::module_name_repetitions)]
//! Per-step lease tokens (P9.2, N7.6 step 2).
//!
//! A [`LeaseRecord`] captures the canonical winner of a race to lease a step:
//! the producer with the lexicographically largest `(lamport, actor.value())`
//! pair. Records live inside [`crate::plan::Plan`] and are surfaced to callers
//! via [`crate::plan::Plan::lease_holder`] and
//! [`crate::plan::Plan::lease_outcome`].
//!
//! Expired leases (those with `expires_at_ms <= now_ms`) are filtered out of
//! `lease_holder` but remain in the fold state — re-applying the same op log
//! must produce identical state regardless of wall-clock, so we never drop
//! records during fold.

use crate::lamport::{ActorId, Lamport};

/// Folded state for a single step's lease.
///
/// Stored inside [`crate::plan::Plan`]; the [`crate::ops::LeaseStep`] op is
/// what produces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LeaseRecord {
    /// Lamport of the winning lease op.
    pub lamport: Lamport,
    /// Actor that issued the winning lease op.
    pub actor: ActorId,
    /// Wall-clock expiry of the winning lease op.
    pub expires_at_ms: u64,
}

impl LeaseRecord {
    /// Construct a record.
    #[must_use]
    pub const fn new(lamport: Lamport, actor: ActorId, expires_at_ms: u64) -> Self {
        Self {
            lamport,
            actor,
            expires_at_ms,
        }
    }

    /// Returns `true` if `self` should replace `other` as the lease holder.
    ///
    /// The rule is purely on `(lamport, actor.value())` — `expires_at_ms` is
    /// metadata, not a tiebreaker, because expiry is wall-clock dependent and
    /// would break fold determinism.
    #[must_use]
    pub fn supersedes(&self, other: &Self) -> bool {
        (self.lamport.value(), self.actor.value()) > (other.lamport.value(), other.actor.value())
    }
}

/// Result of querying the fate of a single `LeaseStep` op.
///
/// Computed by [`crate::plan::Plan::lease_outcome`] *after* the relevant ops
/// have been folded. `lease_outcome` is purely a query — the fold itself
/// never errors on competing lease ops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseOutcome {
    /// The queried op is the current canonical lease holder.
    Granted {
        /// The actor holding the lease (always equal to the queried op's actor).
        holder: ActorId,
    },
    /// The queried op lost a race to another lease op.
    Lost {
        /// The actor that won the race.
        winner: ActorId,
    },
    /// The queried op was not a `LeaseStep`.
    NotALease,
}

#[cfg(test)]
mod tests {
    use super::{ActorId, Lamport, LeaseRecord};

    #[test]
    fn supersedes_orders_by_lamport_then_actor() {
        let r1 = LeaseRecord::new(Lamport::new(10), ActorId::new(1), 1_000);
        let r2 = LeaseRecord::new(Lamport::new(11), ActorId::new(1), 1_000);
        let r3 = LeaseRecord::new(Lamport::new(10), ActorId::new(2), 1_000);

        assert!(r2.supersedes(&r1));
        assert!(!r1.supersedes(&r2));
        assert!(r3.supersedes(&r1));
        assert!(!r1.supersedes(&r3));
        // Equality is not "supersedes" — strictly greater.
        assert!(!r1.supersedes(&r1));
    }
}
