//! HNSW approximate-nearest-neighbour index with temporal-decay re-ranking.
//!
//! Wraps [`hnsw_rs::hnsw::Hnsw`] with a `DistDot` metric (returns `1 - dot`
//! for pre-normalised vectors), then re-ranks the shortlist with
//!
//! ```text
//! score = raw_sim * exp(-age_days / decay_tau_days) * cluster_priority * (1 + edge_boost)
//! ```
//!
//! Public ids (`u64`) are used directly as the `usize` data-id passed to `hnsw_rs`;
//! on 64-bit targets `u64 as usize` is lossless.  An `IndexError::Insert` is
//! returned if an id would overflow `usize` on a hypothetical 32-bit target.

use hnsw_rs::anndists::dist::distances::DistDot;
use hnsw_rs::hnsw::Hnsw;
use thiserror::Error;

use crate::EMBED_DIM;

// HNSW construction parameters (spec P6.3).
const HNSW_MAX_NB_CONNECTION: usize = 16;
const HNSW_MAX_ELEMENTS: usize = 10_000;
const HNSW_MAX_LAYER: usize = 16;
const HNSW_EF_CONSTRUCTION: usize = 200;

// ef used during search: must be ≥ k neighbours requested.
// We clamp to at least shortlist_k at call time, but provide a sensible default.
const HNSW_EF_SEARCH: usize = 64;

// ── Public types ─────────────────────────────────────────────────────────────

/// One re-ranked result from [`MemIndex::search`].
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Public id as supplied at [`MemIndex::insert`] time.
    pub id: u64,
    /// Raw cosine similarity recovered from `DistDot` distance (`1 - distance`).
    pub raw_sim: f32,
    /// Age supplied by the `lookup` closure.
    pub age_days: f32,
    /// Cluster priority supplied by the `lookup` closure.
    pub cluster_priority: f32,
    /// Edge boost supplied by the `lookup` closure.
    pub edge_boost: f32,
    /// Final re-ranked score (spec N6.2).
    pub score: f32,
}

/// Options controlling shortlist size, decay, and filtering.
#[derive(Debug, Clone)]
pub struct SearchOpts {
    /// Number of neighbours to fetch from HNSW before re-ranking.
    pub shortlist_k: usize,
    /// Maximum number of results to return after re-ranking.
    pub top_n: usize,
    /// Exponential-decay half-life in days.
    pub decay_tau_days: f32,
    /// When `true`, candidates whose `MetaRow::superseded_by` is `Some` are dropped.
    pub drop_superseded: bool,
}

impl Default for SearchOpts {
    fn default() -> Self {
        Self {
            shortlist_k: 3000,
            top_n: 5,
            decay_tau_days: 30.0,
            drop_superseded: true,
        }
    }
}

/// Errors returned by [`MemIndex`] operations.
// Repeating "Index" in the variant names is intentional for match-arm clarity
// at call sites outside the crate; the module_name_repetitions lint would fire
// on the enum itself, suppressed below.
#[allow(clippy::module_name_repetitions)] // unambiguous from outside the crate
#[derive(Debug, Error)]
pub enum IndexError {
    /// HNSW insert rejected the point.
    #[error("hnsw insert failed: {0}")]
    Insert(String),
    /// HNSW search returned an unexpected result.
    #[error("hnsw search failed: {0}")]
    Search(String),
}

/// Metadata row provided by the caller's `lookup` closure during search.
#[derive(Debug, Clone, Copy)]
pub struct MetaRow {
    /// Age of the memory in days (used for decay scoring).
    pub age_days: f32,
    /// Priority weight assigned by the cluster/topic layer.
    pub cluster_priority: f32,
    /// Additive boost from outbound-edge strength.
    pub edge_boost: f32,
    /// If `Some(id)`, this memory has been superseded by `id`.
    pub superseded_by: Option<u64>,
}

// ── MemIndex ─────────────────────────────────────────────────────────────────

/// HNSW index over `f32` embedding vectors with temporal-decay re-ranking.
///
/// Vectors **must** be L2-normalised before insertion and querying so that
/// `DistDot` (which returns `1 − dot`) is equivalent to `1 − cos_sim`.
///
/// The `'static` lifetime on `Hnsw` is valid here because all inserted data
/// is owned (`Vec<f32>`) rather than memory-mapped slices.
// `MemIndex` repeats the module name `index`; suppressed so callers can write
// `origin_mem::MemIndex` without a redundant rename.
#[allow(clippy::module_name_repetitions)]
pub struct MemIndex {
    /// Inner HNSW graph; uses `DistDot` so lower distance == higher similarity.
    hnsw: Hnsw<'static, f32, DistDot>,
}

impl MemIndex {
    /// Create an empty index with the construction parameters from spec P6.3.
    #[must_use]
    pub fn new() -> Self {
        Self {
            hnsw: Hnsw::new(
                HNSW_MAX_NB_CONNECTION,
                HNSW_MAX_ELEMENTS,
                HNSW_MAX_LAYER,
                HNSW_EF_CONSTRUCTION,
                DistDot {},
            ),
        }
    }

    /// Insert an L2-normalised embedding with its public id.
    ///
    /// The public `u64` id is passed directly to `hnsw_rs` as the `usize` data-id.
    /// On 64-bit targets (`usize` == `u64`) this is lossless; on 32-bit targets
    /// the conversion is checked and an error is returned if the id overflows.
    ///
    /// # Errors
    /// Returns [`IndexError::Insert`] if the id cannot be represented as `usize`.
    pub fn insert(&mut self, id: u64, vec: &[f32; EMBED_DIM]) -> Result<(), IndexError> {
        // Safe on 64-bit; explicit check guards hypothetical 32-bit targets.
        let uid: usize = id
            .try_into()
            .map_err(|_| IndexError::Insert(format!("id {id} exceeds usize on this target")))?;
        self.hnsw.insert((vec.as_slice(), uid));
        Ok(())
    }

    /// Search for the `opts.shortlist_k` nearest neighbours of `query`, then
    /// re-rank by the temporal-decay formula and return the top `opts.top_n`.
    ///
    /// `lookup` maps a public `u64` id to a [`MetaRow`]; candidates for which
    /// `lookup` returns `None` are silently dropped.  When
    /// `opts.drop_superseded` is `true`, candidates with a non-`None`
    /// `MetaRow::superseded_by` are also dropped.
    ///
    /// Results are sorted descending by `score`; ties break by `id` ascending.
    ///
    /// # Errors
    /// Currently returns [`IndexError::Search`] only for future-proofing (`hnsw_rs`
    /// panics rather than propagates errors, so this path is unreachable today).
    pub fn search(
        &self,
        query: &[f32; EMBED_DIM],
        opts: &SearchOpts,
        lookup: impl Fn(u64) -> Option<MetaRow>,
    ) -> Result<Vec<Candidate>, IndexError> {
        // ef_search must be ≥ knbn; clamp upward so the graph is probed widely enough.
        let ef = HNSW_EF_SEARCH.max(opts.shortlist_k);
        let neighbours = self.hnsw.search(query.as_slice(), opts.shortlist_k, ef);

        let mut candidates: Vec<Candidate> = neighbours
            .into_iter()
            .filter_map(|nb| {
                // Recover public id from the usize data-id stored in hnsw_rs.
                // The cast is lossless on 64-bit (usize == u64); on 32-bit targets
                // all inserted ids fit usize by construction (insert checks this).
                #[allow(clippy::cast_possible_truncation)] // see above
                let id = nb.d_id as u64;

                let row = lookup(id)?; // drop if no metadata

                // Drop superseded entries when requested.
                if opts.drop_superseded && row.superseded_by.is_some() {
                    return None;
                }

                // DistDot returns 1 − dot for normalised vectors → invert to get similarity.
                let raw_sim = 1.0_f32 - nb.distance;

                // Spec N6.2 re-rank formula.
                let score = raw_sim
                    * (-row.age_days / opts.decay_tau_days).exp()
                    * row.cluster_priority
                    * (1.0 + row.edge_boost);

                Some(Candidate {
                    id,
                    raw_sim,
                    age_days: row.age_days,
                    cluster_priority: row.cluster_priority,
                    edge_boost: row.edge_boost,
                    score,
                })
            })
            .collect();

        // Stable sort: descending score, ascending id on tie.
        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        candidates.truncate(opts.top_n);

        Ok(candidates)
    }
}

impl Default for MemIndex {
    fn default() -> Self {
        Self::new()
    }
}
