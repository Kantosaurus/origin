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

        if let Some((evicted_hash, evicted_bytes)) = inner.hot.push(h, bytes.to_vec()) {
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
                if inner.warm_bytes >= inner.cfg.warm_pack_target_bytes {
                    flush_warm(&mut inner)?;
                }
            }
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
                let dec = zstd::decode_all(slice.as_ref()).map_err(|e| StoreError::Zstd(e.to_string()))?;
                return Ok(Some(dec));
            }
        }
        Ok(None)
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

        let compressed = zstd::encode_all(&bytes[..], inner.cfg.cold_zstd_level)
            .map_err(|e| StoreError::Zstd(e.to_string()))?;
        let next_idx = inner.cold_packs.len();
        let path = inner.cfg.root.join("cold").join(format!("c{next_idx:08}.pack"));
        let mut b = PackBuilder::create(&path)?;
        b.append(h, &compressed)?;
        let _ = b.finalize()?;
        let r = PackReader::open(&path)?;
        inner.cold_index.insert(h, next_idx);
        inner.cold_packs.push(r);
        Ok(())
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

fn flush_warm(inner: &mut Inner) -> Result<(), StoreError> {
    if inner.warm_pending.is_empty() {
        return Ok(());
    }
    let next_idx = inner.warm_packs.len();
    let path = inner.cfg.root.join("warm").join(format!("w{next_idx:08}.pack"));
    let mut b = PackBuilder::create(&path)?;
    let pending = std::mem::take(&mut inner.warm_pending);
    for (h, bytes) in pending {
        b.append(h, &bytes)?;
        inner.warm_index.insert(h, next_idx);
    }
    let _ = b.finalize()?;
    let r = PackReader::open(&path)?;
    inner.warm_packs.push(r);
    inner.warm_bytes = 0;
    Ok(())
}
