//! Context injection: embed a user prompt, search the memory index, and format
//! matching memories into a `<context>` block for the system-prompt Sticky band.

use std::sync::Arc;

use parking_lot::RwLock;
use thiserror::Error;

use crate::index::{MemIndex, MetaRow, SearchOpts};
use crate::storage::{MemoryId, MemoryStore};
use crate::{Embedder, EMBED_DIM};

/// Minimum re-ranked score for a candidate to be included in the injected block.
const MIN_SCORE: f32 = 0.2;

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

        // Step 2 — search the index.
        // The daemon (P6.9) maintains `id_map: HashMap<u64, MemoryId>`; for P6.6 we
        // pass a lookup that always returns None so the empty-index path is exercised
        // without full daemon wiring.
        // TODO(P6.9): daemon passes a real lookup closure resolving u64 → MemoryId.
        let opts = SearchOpts {
            top_n: k,
            ..SearchOpts::default()
        };
        let hits = self
            .index
            .read()
            .search(&query, &opts, |_id| -> Option<MetaRow> { None })?;

        // Step 3 — filter by minimum score and return early if nothing qualifies.
        let survivors: Vec<_> = hits.into_iter().filter(|c| c.score >= MIN_SCORE).collect();
        if survivors.is_empty() {
            return Ok(None);
        }

        // Step 4 + 5 — fetch records and build the block.
        // This path is unreachable in P6.6 because the lookup closure above always
        // returns None (making `survivors` always empty).  Full implementation is in P6.9.
        // TODO(P6.9): resolve candidates via `id_map` → `MemoryStore::get` → format lines.
        self.build_context(&survivors)
    }

    /// Build the `<context>` block from a non-empty survivor list.
    ///
    /// In P6.6 this is unreachable (all lookup calls return `None` so `survivors`
    /// is always empty before this is called).  Full wiring happens in P6.9.
    ///
    /// # Errors
    /// Will propagate [`InjectorError::Storage`] in P6.9 when record fetching is wired.
    // Lint suppression: Result<_> is intentional — P6.9 will add fallible store lookups.
    // missing_const_for_fn: suppressed because the full P6.9 body won't be const.
    #[allow(clippy::unnecessary_wraps, clippy::missing_const_for_fn)]
    fn build_context(
        &self,
        survivors: &[crate::index::Candidate],
    ) -> Result<Option<InjectedContext>, InjectorError> {
        // TODO(P6.9): iterate survivors, resolve `id_map` → `MemoryId`, call
        // `self.store.get(id)`, call `format_memory_line`, accumulate lines + touched_ids.
        let _ = survivors;
        let _ = &self.store; // referenced so the field is live in this scope
        Ok(None)
    }
}

/// Format a single memory entry line for the `<context>` block.
///
/// ```text
/// <memory id="01J0…ABC" age="1.2d" tags="feedback,terseness">preview text</memory>
/// ```
///
/// Called from the P6.9 `build_context` implementation once the `id_map` is wired.
// Not yet called in P6.6; wired in P6.9.
#[allow(dead_code)]
pub(crate) fn format_memory_line(id: &MemoryId, age_days: f32, tags: &[String], preview: &str) -> String {
    let tags_str = tags.join(",");
    let id_str = id.to_string();
    format!("  <memory id=\"{id_str}\" age=\"{age_days:.1}d\" tags=\"{tags_str}\">{preview}</memory>")
}
