// SPDX-License-Identifier: Apache-2.0
//! Idle consolidation pass: cosine-similarity clustering → Supersedes proposals
//! + lexical contradiction heuristic.
//!
//! One bounded pass inspects up to `max_pairs` candidate pairs. Safe to call
//! repeatedly from a background task.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use regex::Regex;

use crate::index::{IndexError, MemIndex, MetaRow, SearchOpts};
use crate::storage::{MemoryId, MemoryRecord, MemoryStore, StorageError};

/// Outcome of one consolidator pass.
#[derive(Debug, Clone)]
pub struct ConsolidationReport {
    /// (loser, winner) pairs proposed by similarity clustering.
    pub supersedes_proposed: Vec<(MemoryId, MemoryId)>,
    /// Pairs flagged as candidate contradictions (lexical antonym heuristic).
    pub contradictions_flagged: Vec<(MemoryId, MemoryId)>,
    /// Memories whose `cluster_priority` was bumped because they were re-touched.
    pub priority_bumped: u32,
}

/// Errors returned by [`Consolidator`] operations.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, thiserror::Error)]
pub enum ConsolidationError {
    #[error("storage: {0}")]
    Storage(#[from] StorageError),
    #[error("index: {0}")]
    Index(#[from] IndexError),
}

/// Idle consolidator: clusters near-duplicate memories and flags contradictions.
pub struct Consolidator {
    store: Arc<MemoryStore>,
    index: Arc<RwLock<MemIndex>>,
}

impl Consolidator {
    /// Create a new consolidator backed by `store` and `index`.
    #[must_use]
    pub const fn new(store: Arc<MemoryStore>, index: Arc<RwLock<MemIndex>>) -> Self {
        Self { store, index }
    }

    /// One bounded pass (≤ `max_pairs` candidate inspections). Safe to call repeatedly.
    ///
    /// # Errors
    /// Returns [`ConsolidationError::Storage`] or [`ConsolidationError::Index`] on failure.
    ///
    /// # Panics
    /// Panics if the internally compiled regex patterns are invalid (structurally
    /// unreachable — the patterns are compile-time constants).
    pub fn run_pass(&self, max_pairs: usize) -> Result<ConsolidationReport, ConsolidationError> {
        // ── Precompile lexical contradiction patterns once per pass ──────────
        // Matches positive preference markers.
        let re_positive = Regex::new(r"(?i)\b(prefer|like)\b").expect("valid regex");
        // Matches negative preference markers.
        let re_negative = Regex::new(r"(?i)\b(not|never|don't|hate|avoid)\b").expect("valid regex");

        // ── Load quantizer once ──────────────────────────────────────────────
        let quantizer = self.store.load_quantizer()?.ok_or(StorageError::NoQuantizer)?;

        // ── Fetch all memories, sorted ascending by id (deterministic) ───────
        let memories: Vec<MemoryRecord> = self.store.iter_all()?;

        // ── Build ULID ↔ u64 pass-local maps ────────────────────────────────
        // The HNSW index is keyed by the STABLE id `memory_id_to_u64(&id)` (the
        // high 64 bits of the ULID) — the same scheme the daemon's injector and
        // memory_wiring use to populate it. The previous positional `iter_all`
        // index disagreed with that scheme, so every candidate `.get(id)` missed
        // and supersede/contradiction/priority-bump silently targeted nothing
        // (or, if an index were keyed positionally elsewhere, the WRONG rows).
        // Keep one canonical id scheme across injector, search handle, and here.
        let id_to_u64: HashMap<MemoryId, u64> = memories
            .iter()
            .map(|r| (r.id, crate::memory_id_to_u64(&r.id)))
            .collect();

        // Resolve a candidate's stable u64 back to its record. Keyed by
        // `memory_id_to_u64`, NOT positional order.
        let u64_to_record: HashMap<u64, &MemoryRecord> = memories
            .iter()
            .map(|r| (crate::memory_id_to_u64(&r.id), r))
            .collect();

        // ── Current time for age calculation ────────────────────────────────
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));

        // ── Main pass ────────────────────────────────────────────────────────
        let search_opts = SearchOpts {
            top_n: 3,
            shortlist_k: 30,
            drop_superseded: false,
            decay_tau_days: 30.0,
        };

        let mut supersedes_proposed: Vec<(MemoryId, MemoryId)> = Vec::new();
        let mut contradictions_flagged: Vec<(MemoryId, MemoryId)> = Vec::new();
        let mut priority_bumped: u32 = 0;

        'outer: for record_m in &memories {
            // Stop when bound is reached.
            if supersedes_proposed.len() + contradictions_flagged.len() >= max_pairs {
                break;
            }

            // Decode quantized vector back to f32.
            let vec_m = quantizer.decode(&record_m.encoded);

            // Build lookup closure: stable u64 id → MetaRow.
            let lookup = |uid: u64| -> Option<MetaRow> {
                // `uid` is `memory_id_to_u64(&id)`, the same key the HNSW index
                // is populated with; resolve it through the stable-id map.
                let r = u64_to_record.get(&uid)?;
                let age_ms = now_ms.saturating_sub(r.created_at_ms);
                // Precision loss acceptable: age in days is bounded and the
                // f32 range is sufficient for the decay formula.
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                let age_days = (age_ms as f64 / f64::from(crate::MS_PER_DAY)) as f32;
                Some(MetaRow {
                    age_days,
                    cluster_priority: r.cluster_priority,
                    edge_boost: 0.0,
                    superseded_by: r.superseded_by.and_then(|sid| id_to_u64.get(&sid).copied()),
                })
            };

            let candidates = self.index.read().search(&vec_m, &search_opts, lookup)?;

            let mut peers_touched: u32 = 0;

            for candidate in &candidates {
                // Stop overall when bound is reached.
                if supersedes_proposed.len() + contradictions_flagged.len() >= max_pairs {
                    break 'outer;
                }

                // Skip self.
                let Some(&m_uid) = id_to_u64.get(&record_m.id) else {
                    continue;
                };
                if candidate.id == m_uid {
                    continue;
                }

                // Must exceed similarity threshold.
                if candidate.raw_sim <= 0.92 {
                    continue;
                }

                // Resolve the candidate's stable u64 id back to its record.
                let Some(&record_c) = u64_to_record.get(&candidate.id) else {
                    continue;
                };

                peers_touched += 1;

                // ── Supersede proposal ───────────────────────────────────────
                // If C is older than M, C is the loser (M supersedes C). Persist
                // it so the default `drop_superseded` search path actually elides
                // the stale duplicate — proposing without writing left
                // `superseded_by` NULL forever and the dupe kept being recalled.
                if record_c.created_at_ms < record_m.created_at_ms {
                    self.store.mark_superseded(record_c.id, record_m.id)?;
                    supersedes_proposed.push((record_c.id, record_m.id));
                }

                // ── Lexical contradiction stub ───────────────────────────────
                let body_m = &record_m.body_preview;
                let body_c = &record_c.body_preview;

                let m_pos = re_positive.is_match(body_m);
                let m_neg = re_negative.is_match(body_m);
                let c_pos = re_positive.is_match(body_c);
                let c_neg = re_negative.is_match(body_c);

                // Flag if one body has positive markers and the other has negative.
                if (m_pos && c_neg) || (m_neg && c_pos) {
                    contradictions_flagged.push((record_m.id, record_c.id));
                    // Resolve, don't just log: demote the OLDER contradicting
                    // memory once so the newer view wins recall. The older-than
                    // guard fires this exactly once per pair (when M is the newer
                    // member), mirroring the supersede path above.
                    if record_c.created_at_ms < record_m.created_at_ms {
                        self.store.bump_priority(record_c.id, -0.1)?;
                    }
                }
            }

            // Bump priority if M touched ≥2 peers.
            if peers_touched >= 2 {
                self.store.bump_priority(record_m.id, 0.05)?;
                priority_bumped += 1;
            }
        }

        Ok(ConsolidationReport {
            supersedes_proposed,
            contradictions_flagged,
            priority_bumped,
        })
    }
}
