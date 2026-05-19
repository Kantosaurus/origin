//! Lamport-clock primitives used to totally order op-log events.
//!
//! A [`Lamport`] is a monotonic 64-bit logical timestamp. An [`ActorId`] is the
//! stable producer identifier (coordinator or worker). The pair
//! `(Lamport, ActorId)` is the canonical total order over the op log used by
//! [`crate::fold::fold`].

use core::cmp::Ordering;

/// Stable identifier for a producer of plan ops (coordinator or worker).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActorId(u64);

impl ActorId {
    /// Construct an `ActorId` from a raw 64-bit value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Underlying numeric value.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Logical Lamport timestamp. Monotonic per actor; comparable globally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lamport(u64);

impl Lamport {
    /// Smallest possible timestamp.
    pub const ZERO: Self = Self(0);

    /// Construct a `Lamport` from a raw value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Underlying numeric value.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }

    /// Next monotonic timestamp. Saturates at `u64::MAX` for a degenerate but
    /// safe outcome — no panics on overflow.
    #[must_use]
    pub const fn succ(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// Lamport "observe" rule: on receive, set clock to `max(local, remote) + 1`.
    /// Pure function; callers feed `local` and `remote` and store the result.
    #[must_use]
    pub const fn observe(local: Self, remote: Self) -> Self {
        let m = if local.0 > remote.0 { local.0 } else { remote.0 };
        Self(m.saturating_add(1))
    }
}

/// Canonical total-order key for op-log events: `(lamport, actor)`.
///
/// Used as the sort key in [`crate::fold::fold`] so that any permutation of
/// the input log produces an identical sorted sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OpKey {
    /// Lamport timestamp of the op.
    pub lamport: Lamport,
    /// Producing actor of the op.
    pub actor: ActorId,
}

impl OpKey {
    /// Construct a new op key.
    #[must_use]
    pub const fn new(lamport: Lamport, actor: ActorId) -> Self {
        Self { lamport, actor }
    }
}

impl PartialOrd for OpKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OpKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.lamport.cmp(&other.lamport) {
            Ordering::Equal => self.actor.cmp(&other.actor),
            o => o,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ActorId, Lamport, OpKey};

    #[test]
    fn lamport_observe_picks_max_plus_one() {
        assert_eq!(Lamport::observe(Lamport::new(3), Lamport::new(7)).value(), 8);
        assert_eq!(Lamport::observe(Lamport::new(9), Lamport::new(2)).value(), 10);
    }

    #[test]
    fn opkey_orders_by_lamport_then_actor() {
        let k1 = OpKey::new(Lamport::new(1), ActorId::new(2));
        let k2 = OpKey::new(Lamport::new(1), ActorId::new(3));
        let k3 = OpKey::new(Lamport::new(2), ActorId::new(1));
        assert!(k1 < k2);
        assert!(k2 < k3);
    }
}
