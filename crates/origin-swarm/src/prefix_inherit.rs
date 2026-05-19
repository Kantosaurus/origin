//! `PrefixSnapshot` — coordinator's Frozen+Sticky band assignments,
//! handed to each worker so its first request reuses the parent's
//! `CachePlanner` prefix bytes (N7.1, P9.7).
//!
//! P9.6 shipped an empty stub. P9.7 fills in `from_ledger` (filtering on
//! `Band::{Frozen, Sticky}`) and wires it through `Coordinator::spawn` so
//! every `WorkerContext::inherited_ledger` reflects the parent's stable
//! bands at spawn time. The public surface from P9.6 (`new`, `len`,
//! `is_empty`, `seed_into`) is preserved verbatim — P9.7 only widens the
//! contract by populating the entries vector.

use origin_planner::{Band, PrefixLedger, SectionId};

/// Coordinator → worker `PrefixLedger` snapshot (N7.1).
///
/// Carries only `Frozen` and `Sticky` entries — `Sliding`/`Volatile` change
/// often enough that re-seeding them across the coordinator/worker boundary
/// would be wasted work (the planner re-derives them from this turn's input).
#[derive(Debug, Clone, Default)]
pub struct PrefixSnapshot {
    entries: Vec<(SectionId, Band)>,
}

impl PrefixSnapshot {
    /// Empty snapshot — used when no parent ledger has been configured on
    /// the `Coordinator`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a snapshot from a parent `PrefixLedger`, retaining only
    /// `Frozen` and `Sticky` band assignments.
    #[must_use]
    pub fn from_ledger(l: &PrefixLedger) -> Self {
        let entries: Vec<_> = l
            .iter_bands()
            .filter(|(_, b)| matches!(b, Band::Frozen | Band::Sticky))
            .collect();
        Self { entries }
    }

    /// Number of inherited `(section, band)` entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if no entries have been seeded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Read-only view of the inherited entries (diagnostic / advanced uses).
    #[must_use]
    pub fn entries(&self) -> &[(SectionId, Band)] {
        &self.entries
    }

    /// Seed `ledger` with every inherited `(section, band)` pair.
    ///
    /// `PrefixLedger::record_band` is idempotent — re-seeding an existing
    /// section is a no-op, so calling `seed_into` against a partially
    /// populated ledger is safe.
    pub fn seed_into(&self, ledger: &mut PrefixLedger) {
        for (id, band) in &self.entries {
            ledger.record_band(*id, *band);
        }
    }
}
