//! Three-tier content-addressed store.
//!
//! - **Hot:** in-memory LRU of `Vec<u8>`. Bounded by `hot_capacity` entries.
//! - **Warm:** append-only mmap'd pack files on disk (one pending batch is
//!   flushed when `warm_pack_target_bytes` is reached, sealing one pack).
//! - **Cold:** zstd-compressed pack files; same on-disk format as Warm, but
//!   each payload is independently compressed before append.
//!
//! All three tiers resolve under the same `Hash` namespace. `get(h)` walks
//! Hot → Warm-pending → Warm → Cold; the first hit wins. New writes land in
//! Hot; LRU evictions accumulate in a pending batch that flushes into a Warm
//! pack once the size threshold is crossed. `demote_to_cold` recompresses
//! into a single-entry Cold pack.

use crate::{Hash, PackBuilder, PackReader};
use lru::LruCache;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use thiserror::Error;

/// Tunables for [`Store::open`].
#[derive(Debug, Clone)]
pub struct StoreConfig {
    /// Root directory holding `warm/` and `cold/` subdirs.
    pub root: PathBuf,
    /// Max entries kept in Hot. LRU evicts down to this.
    pub hot_capacity: usize,
    /// Soft cap before pending Warm evictions are sealed into a new pack.
    pub warm_pack_target_bytes: u64,
    /// zstd compression level for Cold (typical: 3).
    pub cold_zstd_level: i32,
}

/// Errors returned by [`Store`] operations.
#[derive(Debug, Error)]
pub enum StoreError {
    /// Underlying I/O error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Pack-file layer error (bad magic, truncation, etc.).
    #[error("pack: {0}")]
    Pack(#[from] crate::packfile::PackError),
    /// zstd encode/decode failure; payload corruption or OOM.
    #[error("zstd: {0}")]
    Zstd(String),
    /// `hot_capacity` of 0 is invalid: LRU requires NonZero.
    #[error("hot capacity must be >= 1")]
    BadHotCapacity,
}

struct Inner {
    cfg: StoreConfig,
    hot: LruCache<Hash, Vec<u8>>,
    warm_bytes: u64,
    warm_pending: Vec<(Hash, Vec<u8>)>,
    warm_packs: Vec<PackReader>,
    cold_packs: Vec<PackReader>,
    warm_index: HashMap<Hash, usize>,
    cold_index: HashMap<Hash, usize>,
    active_dict: Option<(crate::dict::DictVersion, Vec<u8>)>,
}

/// Three-tier content-addressed store: Hot (LRU) → Warm (mmap) → Cold (zstd).
pub struct Store {
    inner: Mutex<Inner>,
}

impl Store {
    /// Open / create a store rooted at `cfg.root`. Re-scans `warm/` and `cold/`
    /// subdirectories to rebuild in-memory indices.
    ///
    /// # Errors
    /// Propagates I/O errors; returns [`StoreError::BadHotCapacity`] if
    /// `hot_capacity == 0`.
    pub fn open(cfg: StoreConfig) -> Result<Self, StoreError> {
        let cap = NonZeroUsize::new(cfg.hot_capacity).ok_or(StoreError::BadHotCapacity)?;
        fs::create_dir_all(cfg.root.join("warm"))?;
        fs::create_dir_all(cfg.root.join("cold"))?;

        let warm_dir = cfg.root.join("warm");
        let cold_dir = cfg.root.join("cold");
        let (warm_packs, warm_index) = scan_dir(&warm_dir)?;
        let (cold_packs, cold_index) = scan_dir(&cold_dir)?;

        Ok(Self {
            inner: Mutex::new(Inner {
                cfg,
                hot: LruCache::new(cap),
                warm_bytes: 0,
                warm_pending: Vec::new(),
                warm_packs,
                cold_packs,
                warm_index,
                cold_index,
                active_dict: None,
            }),
        })
    }

    /// Write bytes; returns the content address. Dedupes by hash across all
    /// three tiers (and the pending warm batch).
    ///
    /// # Errors
    /// Propagates I/O errors from a warm-pack flush if eviction triggers one.
    pub fn put(&self, bytes: &[u8]) -> Result<Hash, StoreError> {
        let h = Hash::of(bytes);
        let mut inner = self.inner.lock();

        if inner.hot.contains(&h)
            || inner.warm_index.contains_key(&h)
            || inner.cold_index.contains_key(&h)
            || inner.warm_pending.iter().any(|(ph, _)| *ph == h)
        {
            return Ok(h);
        }

        let need_flush = if let Some((evicted_hash, evicted_bytes)) = inner.hot.push(h, bytes.to_vec()) {
            // `push` returns `Some((k, v))` for the entry pushed out by capacity.
            // It can also return the same key we just inserted if the cache was
            // full and the new key replaced something — in either case, route
            // the evicted payload to the warm pending batch.
            if evicted_hash != h {
                // usize -> u64 is infallible on every target we care about
                // (32/64-bit). Use `try_from` for portability rather than `as`.
                let len = u64::try_from(evicted_bytes.len()).unwrap_or(u64::MAX);
                inner.warm_bytes = inner.warm_bytes.saturating_add(len);
                inner.warm_pending.push((evicted_hash, evicted_bytes));
                inner.warm_bytes >= inner.cfg.warm_pack_target_bytes
            } else {
                false
            }
        } else {
            false
        };
        if need_flush {
            // Pull pack path + payloads out under the lock, then RELEASE the
            // parking_lot guard before invoking the (possibly uring-backed)
            // writer. We re-acquire the lock to record the new pack reader.
            let next_idx = inner.warm_packs.len();
            let path = inner.cfg.root.join("warm").join(format!("w{next_idx:08}.pack"));
            let pending = std::mem::take(&mut inner.warm_pending);
            drop(inner);
            write_pack(&path, &pending)?;
            let r = PackReader::open(&path)?;
            let mut inner = self.inner.lock();
            for (ph, _) in &pending {
                inner.warm_index.insert(*ph, next_idx);
            }
            inner.warm_packs.push(r);
            inner.warm_bytes = 0;
        }
        Ok(h)
    }

    /// Read bytes by handle. Walks Hot → Warm-pending → Warm → Cold.
    ///
    /// # Errors
    /// Propagates I/O / zstd errors; `Ok(None)` if the hash is unknown.
    pub fn get(&self, h: Hash) -> Result<Option<Vec<u8>>, StoreError> {
        let mut inner = self.inner.lock();
        if let Some(v) = inner.hot.get(&h) {
            return Ok(Some(v.clone()));
        }
        for (ph, pv) in &inner.warm_pending {
            if *ph == h {
                return Ok(Some(pv.clone()));
            }
        }
        if let Some(&idx) = inner.warm_index.get(&h) {
            if let Some(slice) = inner.warm_packs[idx].read(h) {
                return Ok(Some(slice.as_ref().to_vec()));
            }
        }
        if let Some(&idx) = inner.cold_index.get(&h) {
            if let Some(slice) = inner.cold_packs[idx].read(h) {
                let dict_bytes: Option<Vec<u8>> = inner.active_dict.as_ref().map(|(_, d)| d.clone());
                let raw: Vec<u8> = slice.as_ref().to_vec();
                // Release lock before decompression.
                drop(inner);
                let decoded = if let Some(dict) = &dict_bytes {
                    use std::io::Read;
                    use zstd::stream::Decoder;
                    let cursor = std::io::Cursor::new(raw.as_slice());
                    let dec_result = (|| -> Result<Vec<u8>, std::io::Error> {
                        let mut d = Decoder::with_dictionary(cursor, dict)?;
                        let mut buf = Vec::new();
                        d.read_to_end(&mut buf)?;
                        Ok(buf)
                    })();
                    match dec_result {
                        Ok(bytes) => bytes,
                        Err(_) => {
                            zstd::decode_all(raw.as_slice()).map_err(|e| StoreError::Zstd(e.to_string()))?
                        }
                    }
                } else {
                    zstd::decode_all(raw.as_slice()).map_err(|e| StoreError::Zstd(e.to_string()))?
                };
                return Ok(Some(decoded));
            }
        }
        Ok(None)
    }

    /// Flush any pending warm-tier bytes to disk as a fresh warm pack. No-op
    /// if `warm_pending` is empty. Useful at shutdown so unflushed bytes
    /// survive a daemon restart instead of being dropped from RAM only.
    ///
    /// # Errors
    /// Propagates I/O errors from the pack write.
    pub fn flush_warm_pending(&self) -> Result<(), StoreError> {
        let mut inner = self.inner.lock();
        if inner.warm_pending.is_empty() {
            return Ok(());
        }
        let next_idx = inner.warm_packs.len();
        let path = inner.cfg.root.join("warm").join(format!("w{next_idx:08}.pack"));
        let pending = std::mem::take(&mut inner.warm_pending);
        // Release the lock while doing the (possibly uring-backed) write.
        drop(inner);
        write_pack(&path, &pending)?;
        let r = PackReader::open(&path)?;
        let mut inner = self.inner.lock();
        for (ph, _) in &pending {
            inner.warm_index.insert(*ph, next_idx);
        }
        inner.warm_packs.push(r);
        inner.warm_bytes = 0;
        Ok(())
    }

    /// Force `h` to migrate Hot/Warm-pending/Warm → Cold (zstd-compressed pack).
    /// No-op if `h` is already cold or unknown.
    ///
    /// # Errors
    /// Propagates I/O and zstd errors.
    pub fn demote_to_cold(&self, h: Hash) -> Result<(), StoreError> {
        let mut inner = self.inner.lock();
        if inner.cold_index.contains_key(&h) {
            return Ok(());
        }
        let bytes = if let Some(v) = inner.hot.pop(&h) {
            v
        } else if let Some(pos) = inner.warm_pending.iter().position(|(ph, _)| *ph == h) {
            let (_, v) = inner.warm_pending.remove(pos);
            let len = u64::try_from(v.len()).unwrap_or(0);
            inner.warm_bytes = inner.warm_bytes.saturating_sub(len);
            v
        } else if let Some(&idx) = inner.warm_index.get(&h) {
            match inner.warm_packs[idx].read(h) {
                Some(s) => s.as_ref().to_vec(),
                None => return Ok(()),
            }
        } else {
            return Ok(());
        };

        let cold_level = inner.cfg.cold_zstd_level;
        let dict_bytes: Option<Vec<u8>> = inner.active_dict.as_ref().map(|(_, d)| d.clone());
        let next_idx = inner.cold_packs.len();
        let path = inner.cfg.root.join("cold").join(format!("c{next_idx:08}.pack"));
        // Release lock before I/O.
        drop(inner);

        let compressed = if let Some(dict) = &dict_bytes {
            use std::io::Write;
            use zstd::stream::Encoder;
            let mut enc = Encoder::with_dictionary(Vec::new(), cold_level, dict)
                .map_err(|e| StoreError::Zstd(e.to_string()))?;
            enc.write_all(&bytes)
                .map_err(|e| StoreError::Zstd(e.to_string()))?;
            enc.finish().map_err(|e| StoreError::Zstd(e.to_string()))?
        } else {
            zstd::encode_all(&bytes[..], cold_level).map_err(|e| StoreError::Zstd(e.to_string()))?
        };

        let mut b = PackBuilder::create(&path)?;
        b.append(h, &compressed)?;
        let _ = b.finalize()?;
        let r = PackReader::open(&path)?;

        let mut inner = self.inner.lock();
        inner.cold_index.insert(h, next_idx);
        inner.cold_packs.push(r);
        Ok(())
    }

    /// Train a dict from up to `n_samples` decoded cold-tier shards and
    /// persist it under the store root. Subsequent cold writes use this dict.
    ///
    /// # Errors
    /// Propagates `DictError` (wrapped via `StoreError::Zstd`) on training
    /// failure and `StoreError::Io` on file write failure.
    pub fn train_dict_from_sample(&self, n_samples: usize) -> Result<crate::dict::DictVersion, StoreError> {
        let samples = self.collect_samples(n_samples)?;
        let dict_bytes = crate::dict::train(&samples).map_err(|e| StoreError::Zstd(e.to_string()))?;
        let v = self.next_dict_version();
        let root = self.inner.lock().cfg.root.clone();
        let dict_path = root.join(format!("dict-v{}.zstd", v.0));
        std::fs::write(&dict_path, &dict_bytes)?;
        let meta_path = root.join("dict_meta");
        std::fs::write(meta_path, v.0.to_string())?;
        self.inner.lock().active_dict = Some((v, dict_bytes));
        Ok(v)
    }

    /// Return the currently active dictionary version, if any.
    #[must_use]
    pub fn active_dict_version(&self) -> Option<crate::dict::DictVersion> {
        self.inner.lock().active_dict.as_ref().map(|(v, _)| *v)
    }

    fn collect_samples(&self, n: usize) -> Result<Vec<Vec<u8>>, StoreError> {
        // Collect raw byte slices while holding the lock; cold slices are
        // zstd-compressed (needs_decomp=true), warm slices are raw.
        // We release the lock before doing the actual decompression I/O.
        let raw_slices: Vec<(Vec<u8>, bool)> = {
            let inner = self.inner.lock();
            let mut slices = Vec::new();
            // Prefer cold-tier (compressed) samples first.
            for (h, &pack_idx) in &inner.cold_index {
                if slices.len() >= n {
                    break;
                }
                if let Some(slice) = inner.cold_packs[pack_idx].read(*h) {
                    slices.push((slice.as_ref().to_vec(), true));
                }
            }
            // Fall back to warm-tier (uncompressed) samples when cold is sparse.
            for (h, &pack_idx) in &inner.warm_index {
                if slices.len() >= n {
                    break;
                }
                if let Some(slice) = inner.warm_packs[pack_idx].read(*h) {
                    slices.push((slice.as_ref().to_vec(), false));
                }
            }
            // Also include the hot / warm-pending in-memory items.
            for (_, v) in inner.warm_pending.iter().take(n.saturating_sub(slices.len())) {
                slices.push((v.clone(), false));
            }
            slices
        };
        let mut samples = Vec::with_capacity(raw_slices.len());
        for (raw, needs_decomp) in raw_slices {
            let dec = if needs_decomp {
                zstd::decode_all(raw.as_slice()).map_err(|e| StoreError::Zstd(e.to_string()))?
            } else {
                raw
            };
            samples.push(dec);
        }
        Ok(samples)
    }

    fn next_dict_version(&self) -> crate::dict::DictVersion {
        let cur = self.inner.lock().active_dict.as_ref().map_or(0, |(v, _)| v.0);
        crate::dict::DictVersion(cur + 1)
    }
}

fn scan_dir(dir: &std::path::Path) -> Result<(Vec<PackReader>, HashMap<Hash, usize>), StoreError> {
    let mut packs = Vec::new();
    let mut index = HashMap::new();
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let r = PackReader::open(&path)?;
        let idx = packs.len();
        for h in r.hashes() {
            index.insert(h, idx);
        }
        packs.push(r);
    }
    Ok((packs, index))
}

/// Write `pending` payloads to a pack file at `path`. Must be called with **no
/// `Store::Inner` lock held**: on Linux with the `uring` feature this enters a
/// blocking thread that hosts its own `tokio_uring` runtime, and holding a
/// `parking_lot` guard across that hop would (a) deadlock against any other
/// reader and (b) interact badly with the surrounding Tokio runtime that
/// owns the caller.
fn write_pack(path: &std::path::Path, pending: &[(Hash, Vec<u8>)]) -> Result<(), StoreError> {
    // On Linux with the `uring` cargo feature, route the pack flush through
    // the io_uring writer. Everywhere else, fall back to the std BufWriter
    // path that `PackBuilder` already implements.
    #[cfg(all(target_os = "linux", feature = "uring"))]
    {
        // `tokio_uring::start` panics if invoked from inside an existing Tokio
        // runtime worker, and `Store::put` is called from Tokio workers in the
        // daemon. We use `block_in_place` + `spawn_blocking` to land the uring
        // entry on a dedicated OS thread that is *not* a Tokio worker — which
        // is the contract `tokio_uring::start` requires.
        let path_for_writer = path.to_path_buf();
        let pending_for_writer = pending.to_vec();
        // The uring write hops off the Tokio worker pool onto a dedicated OS
        // thread (`tokio_uring::start` panics if called from a Tokio worker).
        // Classify the wrapper as `Background` so the per-class semaphore
        // enforces the budget contract from P12.3.
        let res: Result<(), crate::packfile::PackError> = tokio::task::block_in_place(|| {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(async move {
                origin_runtime::spawn_in(origin_runtime::TaskClass::Background, async move {
                    // Inner `spawn_blocking` is the actual off-worker hop;
                    // `spawn_in` only attaches the class permit + budget.
                    use tokio::task::spawn_blocking as sb;
                    sb(move || {
                        tokio_uring::start(async move {
                            crate::packfile_uring::write_payloads_uring(&path_for_writer, &pending_for_writer)
                                .await
                        })
                    })
                    .await
                    .expect("uring write blocking task panicked")
                })
                .await
                .expect("uring outer spawn_in join failed")
            })
        });
        res?;
    }
    #[cfg(not(all(target_os = "linux", feature = "uring")))]
    {
        let mut b = PackBuilder::create(path)?;
        for (h, bytes) in pending {
            b.append(*h, bytes)?;
        }
        let _ = b.finalize()?;
    }
    Ok(())
}
