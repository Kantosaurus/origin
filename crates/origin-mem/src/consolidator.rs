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
        // Position in the sorted Vec is the u64 index the HNSW uses.
        let id_to_u64: HashMap<MemoryId, u64> = memories
            .iter()
            .enumerate()
            .map(|(i, r)| {
                #[allow(clippy::cast_possible_truncation)]
                (r.id, i as u64)
            })
            .collect();

        let u64_to_record: Vec<&MemoryRecord> = memories.iter().collect();

        // ── Current time for age calculation ────────────────────────────────
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);

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

            // Build lookup closure: u64 index → MetaRow.
            let lookup = |uid: u64| -> Option<MetaRow> {
                // On 64-bit targets usize == u64 so this is lossless; on 32-bit
                // targets the index would have been checked at insert time.
                #[allow(clippy::cast_possible_truncation)]
                let r = u64_to_record.get(uid as usize)?;
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

                // u64 → usize: lossless on 64-bit; 32-bit insert guard ensures fits.
                #[allow(clippy::cast_possible_truncation)]
                let Some(record_c) = u64_to_record.get(candidate.id as usize) else {
                    continue;
                };

                peers_touched += 1;

                // ── Supersede proposal ───────────────────────────────────────
                // If C is older than M, C is the loser (M supersedes C).
                if record_c.created_at_ms < record_m.created_at_ms {
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
