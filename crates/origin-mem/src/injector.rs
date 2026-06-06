// SPDX-License-Identifier: Apache-2.0
//! Context injection: embed a user prompt, search the memory index, and format
//! matching memories into a `<context>` block for the system-prompt Sticky band.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use thiserror::Error;

use crate::index::{MemIndex, MetaRow, SearchOpts};
use crate::storage::{MemoryId, MemoryRecord, MemoryStore};
use crate::{Embedder, EMBED_DIM};

/// Minimum re-ranked score for a candidate to be included in the injected block.
const MIN_SCORE: f32 = 0.2;

use crate::MS_PER_DAY;

// ── Public types ──────────────────────────────────────────────────────────────

/// Pre-formatted context block ready to prepend to the system prompt.
#[derive(Debug, Clone)]
pub struct InjectedContext {
    /// Pre-formatted `<context>` block; ready to prepend to system prompt's Sticky band.
    pub block: String,
    /// IDs that were injected — daemon bumps `last_seen_at_ms` for these.
    pub touched_ids: Vec<MemoryId>,
}

/// Errors returned by [`Injector`] operations.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum InjectorError {
    #[error("embedder: {0}")]
    Embedder(#[from] crate::embedder::EmbedderError),
    #[error("index: {0}")]
    Index(#[from] crate::index::IndexError),
    #[error("storage: {0}")]
    Storage(#[from] crate::storage::StorageError),
}

// ── Injector ──────────────────────────────────────────────────────────────────

/// Embeds a prompt, searches the HNSW index, and formats matching memories.
pub struct Injector {
    embedder: Arc<Embedder>,
    index: Arc<RwLock<MemIndex>>,
    /// Memory store — used in P6.9's populated path to fetch full records.
    store: Arc<MemoryStore>,
}

impl Injector {
    /// Construct a new [`Injector`] from shared handles.
    #[must_use]
    pub const fn new(embedder: Arc<Embedder>, index: Arc<RwLock<MemIndex>>, store: Arc<MemoryStore>) -> Self {
        Self {
            embedder,
            index,
            store,
        }
    }

    /// Embed `user_prompt`, search the top-`k` memories, and return a formatted
    /// `<context>` block.
    ///
    /// Returns `Ok(None)` when the index is empty or no candidate scores above
    /// [`MIN_SCORE`] (0.2).
    ///
    /// # Errors
    /// Propagates embedder, index, and storage errors.
    pub fn for_prompt(&self, user_prompt: &str, k: usize) -> Result<Option<InjectedContext>, InjectorError> {
        // Step 1 — embed the prompt.
        let vec: Vec<f32> = self.embedder.embed(user_prompt)?;

        // Convert Vec<f32> → [f32; EMBED_DIM] for the HNSW search.
        // The embedder guarantees exactly EMBED_DIM elements; if the stub returns
        // a shorter vector we pad with zeros rather than panic.
        let mut query = [0_f32; EMBED_DIM];
        let copy_len = vec.len().min(EMBED_DIM);
        query[..copy_len].copy_from_slice(&vec[..copy_len]);

        // Step 2 — build the u64 → MemoryRecord map (so the HNSW lookup closure
        // can return a MetaRow synchronously). The daemon (P6.9) keeps the index
        // in sync with the store; iterating all rows here is bounded by the
        // HNSW capacity (10k) and runs only at injection time, not per-token.
        let records: Vec<MemoryRecord> = self.store.iter_all()?;
        let by_u64: HashMap<u64, MemoryRecord> = records
            .into_iter()
            .map(|r| (memory_id_to_u64(&r.id), r))
            .collect();
        let now_ms = now_ms_i64();

        // Step 3 — search the index.
        let opts = SearchOpts {
            top_n: k,
            ..SearchOpts::default()
        };
        let hits = self.index.read().search(&query, &opts, |id| -> Option<MetaRow> {
            let r = by_u64.get(&id)?;
            let age_days = age_days_from_created(now_ms, r.created_at_ms);
            Some(MetaRow {
                age_days,
                cluster_priority: r.cluster_priority,
                edge_boost: 0.0,
                superseded_by: r.superseded_by.as_ref().map(memory_id_to_u64),
            })
        })?;

        // Step 4 — filter by minimum score and return early if nothing qualifies.
        let survivors: Vec<_> = hits.into_iter().filter(|c| c.score >= MIN_SCORE).collect();
        if survivors.is_empty() {
            return Ok(None);
        }

        // Step 5 — build the block from the (already-fetched) records.
        Ok(Some(self.build_context(&survivors, &by_u64, now_ms)))
    }

    /// Build the `<context>` block from a non-empty survivor list.
    #[allow(clippy::unused_self)] // kept as a method for symmetry with the public `for_prompt` API
    fn build_context(
        &self,
        survivors: &[crate::index::Candidate],
        by_u64: &HashMap<u64, MemoryRecord>,
        now_ms: i64,
    ) -> InjectedContext {
        let mut lines: Vec<String> = Vec::with_capacity(survivors.len());
        let mut touched_ids: Vec<MemoryId> = Vec::with_capacity(survivors.len());
        for c in survivors {
            let Some(rec) = by_u64.get(&c.id) else {
                continue;
            };
            let age_days = age_days_from_created(now_ms, rec.created_at_ms);
            lines.push(format_memory_line(
                &rec.id,
                age_days,
                &rec.tags,
                &rec.body_preview,
            ));
            touched_ids.push(rec.id);
        }
        let block = if lines.is_empty() {
            String::new()
        } else {
            // Deterministic XML-ish template — assert site:
            //   block.contains("<context source=\"origin-mem\">")
            let inner = lines.join("\n");
            format!("<context source=\"origin-mem\">\n{inner}\n</context>")
        };
        InjectedContext { block, touched_ids }
    }
}

/// Map a `MemoryId` (ULID, 128-bit) onto a `u64` slot for the HNSW index.
///
/// We use the high 64 bits of the ULID's big-endian byte form, which contains
/// the millisecond timestamp plus 16 bits of randomness — collision probability
/// inside the 10k-element HNSW capacity is vanishingly small (birthday-bound).
#[must_use]
pub const fn memory_id_to_u64(id: &MemoryId) -> u64 {
    let bytes = id.to_bytes();
    u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

fn now_ms_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[allow(clippy::cast_precision_loss)] // age in days fits comfortably in f32
fn age_days_from_created(now_ms: i64, created_ms: i64) -> f32 {
    let delta_ms = (now_ms - created_ms).max(0);
    (delta_ms as f32) / MS_PER_DAY
}

/// Format a single memory entry line for the `<context>` block.
///
/// ```text
/// <memory id="01J0…ABC" age="1.2d" tags="feedback,terseness">preview text</memory>
/// ```
///
/// Called from `build_context` once the `id_map` is wired.
pub(crate) fn format_memory_line(id: &MemoryId, age_days: f32, tags: &[String], preview: &str) -> String {
    let tags_str = tags.join(",");
    let id_str = id.to_string();
    format!("  <memory id=\"{id_str}\" age=\"{age_days:.1}d\" tags=\"{tags_str}\">{preview}</memory>")
}
