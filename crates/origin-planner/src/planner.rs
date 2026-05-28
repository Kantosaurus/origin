//! `CachePlanner::plan` — sort sections into Frozen→Sticky→Sliding→Volatile
//! and emit marker positions at every adjacent-band boundary.
#![allow(clippy::module_name_repetitions)]

use crate::{Band, PrefixLedger, SectionId};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, RwLock};

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
///
/// `Plan` is cheaply cloneable: the `ordered`/`markers` vectors are deep-cloned
/// (small) but `handle_bands` and `dynamic_message_markers` are `Arc<RwLock<…>>`,
/// so every clone of the same `Plan` shares those interior-mutable state slots.
/// This is the N4.3 wiring contract: the daemon and the wire-encoder hold
/// separate `Plan` values that point at the same underlying state, so writes
/// on one side are immediately visible to the other without any explicit channel.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    ordered: Vec<Section>,
    markers: Vec<usize>,
    /// N4.3 handle→band index. Populated by `register_handle` after the
    /// planner emits the section order; consulted by the message-to-wire
    /// encoder in `O(1)` per CAS handle to decide `Inline` vs `Reference`.
    ///
    /// Wrapped in `Arc<RwLock<…>>` so the daemon's tool-result dispatch path
    /// (writer) and the provider's wire-encoder (reader) can share the same
    /// map through their respective `Plan` clones — exposing interior
    /// mutability through `&self` on `register_handle` keeps the public API
    /// ergonomic for both call sites.
    ///
    /// Competitor stacks (openclaude/jcode/opencode) re-serialize full
    /// tool-result bytes every turn unconditionally — they have no
    /// per-handle band assignment, so they cannot demote long-lived
    /// handles to reference form. This map is the novel mechanism.
    handle_bands: Arc<RwLock<HashMap<[u8; 32], Band>>>,
    /// Dynamic per-message cache breakpoints populated by the agent loop
    /// each turn. Each index is a position in the session's message list;
    /// the Anthropic wire encoder emits `cache_control` on the last emitting
    /// block of any message whose index appears here.
    ///
    /// Shared via `Arc<RwLock<…>>` so writes from the daemon's per-turn
    /// helper propagate to the provider's wire-encoder, matching the
    /// `register_handle`/`band_for_handle` ergonomics on `handle_bands`.
    dynamic_message_markers: Arc<RwLock<Vec<usize>>>,
}

impl PartialEq for Plan {
    fn eq(&self, other: &Self) -> bool {
        if self.ordered != other.ordered || self.markers != other.markers {
            return false;
        }
        // Compare map contents under read locks. Identical Arc pointers
        // short-circuit cheaply; distinct Arcs fall through to per-entry
        // equality. Lock poisoning on either side defaults to "not equal"
        // rather than panicking — equality is a query, not a state mutation.
        match (
            self.handle_bands.read(),
            other.handle_bands.read(),
            self.dynamic_message_markers.read(),
            other.dynamic_message_markers.read(),
        ) {
            (Ok(a), Ok(b), Ok(c), Ok(d)) => *a == *b && *c == *d,
            _ => false,
        }
    }
}

impl Eq for Plan {}

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

    /// Register a CAS handle with the band it lives in for this turn.
    ///
    /// Subsequent serialization passes (e.g. the Anthropic provider's
    /// `expand_messages_for_wire`) consult `band_for_handle` to skip
    /// inlining for handles parked in a non-Volatile band whose body
    /// exceeds `INLINE_BYTE_BUDGET`.
    ///
    /// Idempotent: re-registering the same handle overwrites its band.
    /// Takes `&self` (not `&mut self`) so the daemon's per-turn dispatch
    /// path and the provider's wire-encoder can both write through their
    /// own `Plan` clones; the underlying map is shared via `Arc<RwLock<…>>`.
    pub fn register_handle(&self, handle: [u8; 32], band: Band) {
        let mut guard = self
            .handle_bands
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.insert(handle, band);
    }

    /// `O(1)` lookup of the band a CAS handle has been parked in.
    ///
    /// Returns `None` when the handle has not been registered. Callers
    /// must treat `None` as "no information" and fall back to the safe
    /// floor (`Band::Volatile`) — never assume a missing entry means
    /// the handle is stable.
    #[must_use]
    pub fn band_for_handle(&self, handle: &[u8; 32]) -> Option<Band> {
        let guard = self
            .handle_bands
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(handle).copied()
    }

    /// Number of registered CAS handles. Exposed for test assertions and
    /// telemetry — callers should not rely on a particular value at any
    /// specific point in the request lifecycle.
    #[must_use]
    pub fn handle_count(&self) -> usize {
        let guard = self
            .handle_bands
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.len()
    }

    /// Current dynamic per-message cache breakpoints, populated by the
    /// agent loop via [`Plan::set_dynamic_message_markers`].
    ///
    /// Returns an owned `Vec` rather than a borrowed slice so callers don't
    /// hold a read lock across an arbitrarily long encode pass. Empty by
    /// default; the wire-encoder treats the empty case as "no extra markers"
    /// and only emits via the legacy `marker_indices` and per-block paths.
    #[must_use]
    pub fn dynamic_message_markers(&self) -> Vec<usize> {
        self.dynamic_message_markers
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Replace the dynamic per-message cache breakpoints. Writes through
    /// `&self` so the agent loop can update without holding a `&mut Plan` —
    /// matching the `register_handle` ergonomics on `handle_bands`.
    pub fn set_dynamic_message_markers(&self, indices: Vec<usize>) {
        let mut guard = self
            .dynamic_message_markers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = indices;
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
        Plan {
            ordered,
            markers,
            handle_bands: Arc::new(RwLock::new(HashMap::new())),
            dynamic_message_markers: Arc::new(RwLock::new(Vec::new())),
        }
    }
}
