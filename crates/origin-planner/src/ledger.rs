#![allow(clippy::module_name_repetitions)]

//! `PrefixLedger` — per-section stability scoring.
//!
//! Each `(section_id, band)` carries a running `Stability` score updated by
//! `record_hit` (positive) and `record_miss` (negative). When the score crosses
//! `PROMOTE_THRESHOLD` the section is promoted one band toward Frozen; when it
//! crosses `DEMOTE_THRESHOLD` it is demoted one band toward Volatile.

use crate::Band;
use std::collections::HashMap;
use thiserror::Error;

/// Score threshold above which a section is promoted (closer to Frozen).
pub const PROMOTE_THRESHOLD: i32 = 3;
/// Score threshold below which a section is demoted (closer to Volatile).
pub const DEMOTE_THRESHOLD: i32 = -2;

/// Stable identifier for a request section. Cheap to clone; semantically
/// opaque to the planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SectionId(&'static str);

impl SectionId {
    /// Create a new `SectionId` from a static string slice.
    #[must_use]
    pub const fn new(s: &'static str) -> Self {
        Self(s)
    }

    /// Return the underlying string slice.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// Running stability score for one section.
#[derive(Debug, Clone, Copy)]
pub struct Stability {
    /// Net hits minus misses across the lifetime of this section.
    pub score: i32,
    /// Current band the section is parked in.
    pub band: Band,
}

/// Errors surfaced by `PrefixLedger` queries.
///
/// Defined in P3.1; first returned by `CachePlanner::plan` in P3.2 when the
/// planner asks the ledger about a section it never saw seeded.
#[derive(Debug, Error)]
pub enum LedgerError {
    /// Caller asked for a section the ledger never saw.
    #[error("unknown section: {0}")]
    Unknown(&'static str),
}

/// Per-section stability ledger used by the `CachePlanner`.
#[derive(Debug, Default)]
pub struct PrefixLedger {
    table: HashMap<SectionId, Stability>,
}

impl PrefixLedger {
    /// Create an empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a section with its current band. Idempotent.
    pub fn record_band(&mut self, id: SectionId, band: Band) {
        self.table.entry(id).or_insert(Stability { score: 0, band });
    }

    /// Record a cache hit. `tokens_read` is informational only at this stage; real workloads
    /// will weigh by token count once telemetry lands in a future phase
    /// (no phase tag yet — currently unscheduled). Kept in the signature so callers don't
    /// change once weighting is added.
    pub fn record_hit(&mut self, id: SectionId, _tokens_read: u32) {
        let entry = self.table.entry(id).or_insert(Stability {
            score: 0,
            band: Band::Volatile,
        });
        entry.score = entry.score.saturating_add(1);
        if entry.score >= PROMOTE_THRESHOLD {
            if let Some(b) = entry.band.promoted() {
                entry.band = b;
                entry.score = 0;
            }
        }
    }

    /// Record a cache miss.
    pub fn record_miss(&mut self, id: SectionId) {
        let entry = self.table.entry(id).or_insert(Stability {
            score: 0,
            band: Band::Volatile,
        });
        entry.score = entry.score.saturating_sub(1);
        if entry.score <= DEMOTE_THRESHOLD {
            if let Some(b) = entry.band.demoted() {
                entry.band = b;
                entry.score = 0;
            }
        }
    }

    /// Current band the planner should park this section in.
    #[must_use]
    pub fn suggested_band(&self, id: SectionId) -> Option<Band> {
        self.table.get(&id).map(|s| s.band)
    }

    /// Iterate every `(section_id, band)` currently held by the ledger.
    ///
    /// Read-only view consumed by `origin-swarm` (P9.7, N7.1) to snapshot a
    /// coordinator's stable-band assignments before fanning them out to
    /// worker `WorkerContext`s. Iteration order is unspecified (`HashMap`).
    pub fn iter_bands(&self) -> impl Iterator<Item = (SectionId, Band)> + '_ {
        self.table.iter().map(|(id, s)| (*id, s.band))
    }
}
