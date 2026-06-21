// SPDX-License-Identifier: Apache-2.0
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

    /// Rebuild the in-RAM HNSW index from the persisted store rows (#2).
    ///
    /// A restarted daemon starts with an empty [`MemIndex`]; without this the
    /// HNSW search path in [`MemoryDispatchHandle::search`] stays empty until
    /// new `mem_save` calls land, so prompt-recall over previously-saved
    /// memories silently degrades to the naïve substring scan. This walks every
    /// stored row, decodes its quantized vector back to `f32`, and re-inserts it
    /// keyed by the same `u64` id the search path uses.
    ///
    /// Only runs when an embedder is wired: rows persisted without a real
    /// embedder carry a fallback placeholder vector (see
    /// [`MemoryDispatchHandle::save`]) that must not enter the index. When no
    /// embedder is present this is a no-op and returns `0`. Best-effort: a
    /// missing quantizer (nothing saved yet) is treated as "nothing to
    /// rehydrate" and returns `0` rather than erroring.
    ///
    /// Returns the number of vectors inserted.
    ///
    /// # Errors
    /// Propagates a [`origin_mem::StorageError`] only if iterating the store
    /// fails; per-row insert failures are surfaced as the same error type.
    pub fn rehydrate_index(&self) -> Result<usize, origin_mem::StorageError> {
        if self.embedder.is_none() {
            return Ok(0);
        }
        let Some(quantizer) = self.store.load_quantizer()? else {
            // No quantizer trained yet ⇒ no rows to rehydrate.
            return Ok(0);
        };
        let records = self.store.iter_all()?;
        // Decode every row's vector OUTSIDE the index lock, skipping the
        // fallback placeholder vector ([1,0,0,…]) so a row saved before the
        // embedder was installed never pollutes recall. We then take the write
        // lock only for the tight insert batch (keeps lock scope minimal).
        let to_insert: Vec<(u64, [f32; EMBED_DIM])> = records
            .iter()
            .filter_map(|r| {
                let vec = quantizer.decode(&r.encoded);
                let is_placeholder = vec.iter().skip(1).all(|x| *x == 0.0) && (vec[0] - 1.0).abs() < 1e-6;
                (!is_placeholder).then(|| (origin_mem::memory_id_to_u64(&r.id), vec))
            })
            .collect();
        let inserted = to_insert.len();
        let mut index = self.index.write();
        for (uid, vec) in &to_insert {
            index
                .insert(*uid, vec)
                .map_err(|e| origin_mem::StorageError::QuantizerFormat(e.to_string()))?;
        }
        drop(index);
        Ok(inserted)
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
    fn search(&self, query: &str, k: usize, fresh: bool) -> Result<Vec<SearchHit>, MemoryToolError> {
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
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
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
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));

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
            // Honor `fresh`: prioritise recency over pure relevance by ordering
            // newest-first (the naive path is already age-ordered). Previously the
            // flag was parsed and advertised but silently ignored.
            if fresh {
                out.sort_by(|a, b| {
                    a.age_days
                        .partial_cmp(&b.age_days)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
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
        // Whether a REAL (non-zero) embedding was produced. Only then is the
        // vector meaningful enough to insert into the HNSW index — the fallback
        // unit vector below is a schema placeholder, not a semantic embedding,
        // so indexing it would pollute recall with `[1,0,0,…]` neighbours.
        let mut embedded_real = false;
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
                    embedded_real = true;
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

        // #2: populate the in-RAM HNSW index so prompt-recall (`Injector`) and
        // consolidation can find this row WITHOUT a full rebuild. Before this
        // fix `save()` only wrote the store row, leaving the index empty, so the
        // HNSW search path in `search()` always fell through to the naïve scan.
        // Only insert a real embedding (see `embedded_real`); the fallback unit
        // vector is a schema placeholder and must not enter the index.
        if embedded_real {
            self.index
                .write()
                .insert(origin_mem::memory_id_to_u64(&id), &vec)
                .map_err(|e| MemoryToolError::Storage(e.to_string()))?;
        }
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use origin_mem::{memory_id_to_u64, MetaRow, SearchOpts};

    /// Build a fresh, empty `MemoryWiring` (no embedder) backed by a tempdir
    /// CAS + on-disk `SQLite` (so migrations run). Returns the wiring and the
    /// `TempDir` guard (kept alive for the test's duration).
    fn fresh_wiring() -> (MemoryWiring, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let cas = origin_cas::Store::open(origin_cas::StoreConfig {
            root: tmp.path().join("cas"),
            hot_capacity: 64,
            warm_pack_target_bytes: 1 << 20,
            cold_zstd_level: 3,
        })
        .unwrap();
        let sql = origin_store::Store::open(tmp.path().join("origin.db").to_str().unwrap()).unwrap();
        let store = Arc::new(MemoryStore::new(Arc::new(sql), Arc::new(cas)));
        let index = Arc::new(RwLock::new(MemIndex::new()));
        (MemoryWiring::new(store, None, index), tmp)
    }

    /// #2 core: inserting an embedding into the SHARED index keyed by
    /// `memory_id_to_u64(&id)` makes it searchable. This exercises exactly the
    /// id-keying + `index.insert` + `index.search` round-trip that the
    /// `save()` fix now performs (the production embedder path is ONNX-gated, so
    /// this drives the same shared index Arc directly with a known unit vector).
    #[test]
    fn shared_index_insert_makes_row_searchable() {
        let (wiring, _tmp) = fresh_wiring();

        // A real, L2-normalised vector (not the fallback placeholder).
        let mut vec = [0_f32; EMBED_DIM];
        vec[3] = 0.6;
        vec[7] = 0.8; // 0.6^2 + 0.8^2 = 1.0 ⇒ already unit norm.

        let id = ulid::Ulid::new();
        let uid = memory_id_to_u64(&id);

        // Before insert: the shared index is empty ⇒ search finds nothing.
        let opts = SearchOpts {
            top_n: 5,
            ..SearchOpts::default()
        };
        let pre = wiring
            .index
            .read()
            .search(&vec, &opts, |found| {
                (found == uid).then_some(MetaRow {
                    age_days: 0.0,
                    cluster_priority: 1.0,
                    edge_boost: 0.0,
                    superseded_by: None,
                })
            })
            .unwrap();
        assert!(pre.is_empty(), "empty index must return no candidates");

        // The exact line `save()` now runs on a real embedding.
        wiring.index.write().insert(uid, &vec).unwrap();

        // After insert: querying the same vector returns this id.
        let post = wiring
            .index
            .read()
            .search(&vec, &opts, |found| {
                (found == uid).then_some(MetaRow {
                    age_days: 0.0,
                    cluster_priority: 1.0,
                    edge_boost: 0.0,
                    superseded_by: None,
                })
            })
            .unwrap();
        assert!(
            post.iter().any(|c| c.id == uid),
            "after the save-path insert the row must be searchable in the shared HNSW index; got {post:?}"
        );
    }

    /// #2 rehydrate: with no embedder wired, `rehydrate_index` is a safe no-op
    /// (rows carry only placeholder vectors, which must never enter the index).
    #[test]
    fn rehydrate_index_is_noop_without_embedder() {
        let (wiring, _tmp) = fresh_wiring();
        // Save a row via the handle (no embedder ⇒ fallback placeholder vector).
        let handle = wiring.handle();
        handle.save("a remembered fact", &["test".to_string()]).unwrap();
        // Rehydrate must not insert the placeholder row.
        assert_eq!(
            wiring.rehydrate_index().unwrap(),
            0,
            "rehydrate must be a no-op when no embedder is wired"
        );
    }
}
