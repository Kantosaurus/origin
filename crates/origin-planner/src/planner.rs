//! `CachePlanner::plan` — sort sections into Frozen→Sticky→Sliding→Volatile
//! and emit marker positions at every adjacent-band boundary.
#![allow(clippy::module_name_repetitions)]

use crate::{Band, PrefixLedger, SectionId};
use std::ops::Range;

/// One contiguous portion of the outgoing request. The planner sorts these
/// by `band` and emits cache markers between adjacent bands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Stable identifier for this section.
    pub id: SectionId,
    /// The band this section belongs to.
    pub band: Band,
    /// Byte range inside the section's original block (informational; used by
    /// `WireDecision` in P3.6 to pick inline vs reference).
    pub bytes: Range<usize>,
}

impl Section {
    /// Create a new `Section`.
    #[must_use]
    pub const fn new(id: SectionId, band: Band, bytes: Range<usize>) -> Self {
        Self { id, band, bytes }
    }
}

/// Output of `CachePlanner::plan`. `marker_indices()[i]` means "emit a cache
/// marker after `ordered_sections()[i]`".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    ordered: Vec<Section>,
    markers: Vec<usize>,
}

impl Plan {
    /// Return the sections in canonical band order (Frozen→Sticky→Sliding→Volatile).
    #[must_use]
    pub fn ordered_sections(&self) -> &[Section] {
        &self.ordered
    }

    /// Return the indices after which a cache marker should be emitted.
    /// Each index `i` means: emit a marker after `ordered_sections()[i]`.
    #[must_use]
    pub fn marker_indices(&self) -> &[usize] {
        &self.markers
    }
}

/// Plans the cache-prefix layout for a single request.
pub struct CachePlanner<'a> {
    ledger: &'a PrefixLedger,
}

impl<'a> CachePlanner<'a> {
    /// Create a new `CachePlanner` backed by the given ledger.
    #[must_use]
    pub const fn new(ledger: &'a PrefixLedger) -> Self {
        Self { ledger }
    }

    /// Sort sections into canonical band order and compute marker positions.
    ///
    /// The ledger may override an input section's `band` if the running
    /// stability score has promoted/demoted it. Sections within the same band
    /// retain their caller-supplied order (stable sort).
    #[must_use]
    pub fn plan(&self, sections: &[Section]) -> Plan {
        let mut ordered: Vec<Section> = sections
            .iter()
            .map(|s| {
                let band = self.ledger.suggested_band(s.id).unwrap_or(s.band);
                Section { band, ..s.clone() }
            })
            .collect();
        // Stable sort so sections inside one band stay in caller-supplied order.
        ordered.sort_by_key(|s| s.band as u8);

        let mut markers = Vec::new();
        for (i, pair) in ordered.windows(2).enumerate() {
            if pair[0].band != pair[1].band {
                markers.push(i);
            }
        }
        Plan { ordered, markers }
    }
}
