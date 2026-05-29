//! Memory subsystem wiring for the daemon (P6.9).
//!
//! `MemoryWiring` bundles the `MemoryStore`, `Embedder` (optional — degrades
//! gracefully if the ONNX model isn't installed), HNSW `MemIndex`, `Injector`,
//! and `Consolidator` behind cheap `Arc`s so the daemon's per-connection
//! handler can clone references without re-opening any underlying resource.
//!
//! `MemoryDispatchHandle` adapts the store/index/embedder triple into the
//! object-safe `origin_tools::dispatch::MemoryHandle` trait so the in-process
//! tool dispatch can route `mem_search`/`mem_save`/`mem_forget` to live state
//! without `origin-tools` depending on `origin-mem`.
//!
//! Graceful-degrade contract: when the ONNX embedder is unavailable
//! (`ORIGIN_MEM_MODEL_DIR` unset or model load fails), the daemon still wires
//! the store + a naïve substring search; the `Injector` and `Consolidator` are
//! omitted because both require the embedder. Calls to `mem_search` then use a
//! linear scan over `body_preview`. This keeps `mem_save`/`mem_forget` usable
//! from day-one without forcing every user to install ONNX.

use std::sync::Arc;

use origin_mem::{Consolidator, Embedder, Injector, MemIndex, MemoryStore, Proposer, Quantizer};
use origin_tools::dispatch::{MemoryHandle, MemoryToolError, SearchHit};
use parking_lot::RwLock;
use ulid::Ulid;

use origin_mem::EMBED_DIM;

/// All shared memory-subsystem handles the daemon hands out to per-connection tasks.
#[derive(Clone)]
pub struct MemoryWiring {
    /// Persistent store (`SQLite` + CAS bodies). Always present when wiring succeeds.
    pub store: Arc<MemoryStore>,
    /// Optional ONNX embedder; `None` when the model is not installed.
    pub embedder: Option<Arc<Embedder>>,
    /// In-RAM HNSW index. Empty until `mem_save` calls land or the daemon
    /// rebuilds at startup (out of scope for P6.9).
    pub index: Arc<RwLock<MemIndex>>,
    /// Prompt-recall injector; `None` mirrors `embedder == None`.
    pub injector: Option<Arc<Injector>>,
    /// Idle consolidator; `None` mirrors `embedder == None`.
    pub consolidator: Option<Arc<Consolidator>>,
    /// Proposer (regex-only, cheap, always available).
    pub proposer: Arc<Proposer>,
}

impl MemoryWiring {
    /// Build a [`MemoryWiring`] from already-constructed Arcs.
    #[must_use]
    pub fn new(
        store: Arc<MemoryStore>,
        embedder: Option<Arc<Embedder>>,
        index: Arc<RwLock<MemIndex>>,
    ) -> Self {
        let (injector, consolidator) = embedder.as_ref().map_or((None, None), |emb| {
            let injector = Arc::new(Injector::new(
                Arc::clone(emb),
                Arc::clone(&index),
                Arc::clone(&store),
            ));
            let consolidator = Arc::new(Consolidator::new(Arc::clone(&store), Arc::clone(&index)));
            (Some(injector), Some(consolidator))
        });
        let proposer = Arc::new(Proposer::new());
        Self {
            store,
            embedder,
            index,
            injector,
            consolidator,
            proposer,
        }
    }

    /// Wrap the store + index into a `MemoryHandle` the tool dispatch can use.
    #[must_use]
    pub fn handle(&self) -> Arc<MemoryDispatchHandle> {
        Arc::new(MemoryDispatchHandle {
            store: Arc::clone(&self.store),
            embedder: self.embedder.clone(),
            index: Arc::clone(&self.index),
        })
    }
}

/// `MemoryHandle` impl that adapts the daemon's store/index/embedder triple.
///
/// `search` prefers HNSW (when an embedder is wired); falls back to a naïve
/// substring scan over `body_preview` otherwise. This keeps the tool usable
/// even when the ONNX model isn't installed.
pub struct MemoryDispatchHandle {
    pub(crate) store: Arc<MemoryStore>,
    pub(crate) embedder: Option<Arc<Embedder>>,
    pub(crate) index: Arc<RwLock<MemIndex>>,
}

impl std::fmt::Debug for MemoryDispatchHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryDispatchHandle")
            .field("embedder", &self.embedder.is_some())
            .finish_non_exhaustive()
    }
}

impl MemoryHandle for MemoryDispatchHandle {
    fn search(&self, query: &str, k: usize, _fresh: bool) -> Result<Vec<SearchHit>, MemoryToolError> {
        // Naïve fallback: linear substring scan + age-based ranking.
        // We use the naïve path whenever (a) no embedder, or (b) embed fails.
        // This keeps the daemon usable without ONNX installed.
        let do_naive = || -> Result<Vec<SearchHit>, MemoryToolError> {
            let all = self
                .store
                .iter_all()
                .map_err(|e| MemoryToolError::Storage(e.to_string()))?;
            let q_lower = query.to_lowercase();
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
                .unwrap_or(0);
            let mut hits: Vec<SearchHit> = all
                .into_iter()
                .filter(|r| r.body_preview.to_lowercase().contains(&q_lower))
                .map(|r| {
                    #[allow(clippy::cast_precision_loss)]
                    let age_days = ((now_ms - r.created_at_ms).max(0) as f32) / origin_mem::MS_PER_DAY;
                    SearchHit {
                        id: r.id.to_string(),
                        preview: r.body_preview,
                        score: 1.0,
                        age_days,
                        tags: r.tags,
                    }
                })
                .collect();
            // All naive matches share score 1.0 and `iter_all` yields records
            // oldest-first, so a bare `truncate(k)` would keep the OLDEST k
            // matches and silently drop newer ones. Order newest-first (smallest
            // age) so truncation retains the most recent — and most relevant —
            // matches.
            hits.sort_by(|a, b| {
                a.age_days
                    .partial_cmp(&b.age_days)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            hits.truncate(k);
            Ok(hits)
        };

        // Prefer the HNSW path when an embedder is wired.
        if let Some(emb) = self.embedder.as_ref() {
            let Ok(vec) = emb.embed(query) else {
                return do_naive();
            };
            let mut q_arr = [0_f32; EMBED_DIM];
            let copy_len = vec.len().min(EMBED_DIM);
            q_arr[..copy_len].copy_from_slice(&vec[..copy_len]);

            // Build the u64 -> record map for the lookup closure.
            let records = self
                .store
                .iter_all()
                .map_err(|e| MemoryToolError::Storage(e.to_string()))?;
            let by_u64: std::collections::HashMap<u64, origin_mem::MemoryRecord> = records
                .into_iter()
                .map(|r| (origin_mem::memory_id_to_u64(&r.id), r))
                .collect();
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
                .unwrap_or(0);

            let opts = origin_mem::SearchOpts {
                top_n: k,
                ..origin_mem::SearchOpts::default()
            };
            let candidates = self
                .index
                .read()
                .search(&q_arr, &opts, |id| {
                    let r = by_u64.get(&id)?;
                    #[allow(clippy::cast_precision_loss)]
                    let age_days = ((now_ms - r.created_at_ms).max(0) as f32) / origin_mem::MS_PER_DAY;
                    Some(origin_mem::MetaRow {
                        age_days,
                        cluster_priority: r.cluster_priority,
                        edge_boost: 0.0,
                        superseded_by: r.superseded_by.as_ref().map(origin_mem::memory_id_to_u64),
                    })
                })
                .map_err(|e| MemoryToolError::Storage(e.to_string()))?;

            if candidates.is_empty() {
                // HNSW returned nothing — fall back to substring scan so cold
                // databases (no embedder pipeline run yet) still return hits.
                return do_naive();
            }
            let mut out = Vec::with_capacity(candidates.len());
            for c in candidates {
                if let Some(r) = by_u64.get(&c.id) {
                    out.push(SearchHit {
                        id: r.id.to_string(),
                        preview: r.body_preview.clone(),
                        score: c.score,
                        age_days: c.age_days,
                        tags: r.tags.clone(),
                    });
                }
            }
            return Ok(out);
        }
        do_naive()
    }

    fn save(&self, body: &str, tags: &[String]) -> Result<String, MemoryToolError> {
        // We need a quantizer installed before `MemoryStore::save` accepts
        // anything. For day-one usage without ONNX we lazily install a
        // deterministic fallback quantizer; the embedder isn't used in the
        // naïve search path, so this is purely a schema requirement.
        if self
            .store
            .load_quantizer()
            .map_err(|e| MemoryToolError::Storage(e.to_string()))?
            .is_none()
        {
            ensure_fallback_quantizer(&self.store)?;
        }

        // Embed the body — degrade to a zero vector if no embedder. The naïve
        // search path doesn't use the embedding so this is safe.
        let mut vec = [0_f32; EMBED_DIM];
        if let Some(emb) = self.embedder.as_ref() {
            if let Ok(v) = emb.embed(body) {
                let copy_len = v.len().min(EMBED_DIM);
                vec[..copy_len].copy_from_slice(&v[..copy_len]);
                // Unit-normalise so `Quantizer::encode`'s debug_assert holds.
                let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > 1e-9 {
                    for x in &mut vec {
                        *x /= norm;
                    }
                }
            }
        }
        if vec.iter().all(|x| *x == 0.0) {
            // Default to a deterministic unit vector so the quantizer's
            // debug_assert about non-unit input doesn't fire in test builds.
            vec[0] = 1.0;
        }

        let tag_refs: Vec<&str> = tags.iter().map(String::as_str).collect();
        let id = self
            .store
            .save(body, &vec, &tag_refs)
            .map_err(|e| MemoryToolError::Storage(e.to_string()))?;
        Ok(id.to_string())
    }

    fn forget(&self, id: &str) -> Result<(), MemoryToolError> {
        let ulid = Ulid::from_string(id).map_err(|e| MemoryToolError::BadId(e.to_string()))?;
        self.store
            .forget(ulid)
            .map_err(|e| MemoryToolError::Storage(e.to_string()))
    }
}

/// Install a deterministic fallback quantizer so `MemoryStore::save` accepts
/// rows even when the daemon hasn't trained one from real data yet.
///
/// We synthesise `NUM_CENTROIDS` near-orthogonal vectors by setting one
/// element of each to 1.0 (cycling through dimensions). The quantizer it
/// trains is unsuitable for high-recall search but is fine for the naïve
/// substring fallback path that doesn't use the encoded vector.
fn ensure_fallback_quantizer(store: &MemoryStore) -> Result<(), MemoryToolError> {
    let mut training = Vec::with_capacity(origin_mem::NUM_CENTROIDS);
    for i in 0..origin_mem::NUM_CENTROIDS {
        let mut v = [0_f32; EMBED_DIM];
        v[i % EMBED_DIM] = 1.0;
        training.push(v);
    }
    let q = Quantizer::fit(&training, 0).map_err(|e| MemoryToolError::Storage(e.to_string()))?;
    store
        .install_quantizer(&q)
        .map_err(|e| MemoryToolError::Storage(e.to_string()))?;
    Ok(())
}
