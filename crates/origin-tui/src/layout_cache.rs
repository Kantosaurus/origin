//! CAS-backed per-turn layout cache (P4.8).

use origin_cas::{Hash, Store, StoreError};
use parking_lot::Mutex;
use rkyv::{Archive, Deserialize, Infallible, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation;

use crate::width::WidthCache;

/// One wrapped span: where in the output grid this byte range lands.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[archive(check_bytes)]
pub struct LayoutSpan {
    pub row: u16,
    pub col: u16,
    pub byte_start: u32,
    pub byte_end: u32,
}

/// Errors returned by [`LayoutCache::get_or_build`].
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum LayoutCacheError {
    #[error("cas: {0}")]
    Cas(#[from] StoreError),
    #[error("rkyv decode: {0}")]
    Decode(String),
    #[error("rkyv encode: {0}")]
    Encode(String),
}

/// CAS-backed per-turn layout cache.
///
/// Wraps text at `viewport_cols` columns, archives the resulting
/// [`LayoutSpan`] vector via rkyv, and stores it in an [`origin_cas::Store`].
/// An in-memory hash-map of `short_key → cas_hash` provides fast second-call
/// retrieval without a CAS lookup.
pub struct LayoutCache {
    store: Arc<Store>,
    viewport_cols: u16,
    widths: WidthCache,
    key_index: Mutex<HashMap<u64, Hash>>,
}

impl LayoutCache {
    /// Create a new [`LayoutCache`] backed by `store`, wrapping at `viewport_cols`.
    #[must_use]
    pub fn new(store: Arc<Store>, viewport_cols: u16) -> Self {
        Self {
            store,
            viewport_cols,
            widths: WidthCache::new(8 * 1024),
            key_index: Mutex::new(HashMap::new()),
        }
    }

    /// Compute or recall the wrapped layout of `text` at the cache's configured width.
    ///
    /// # Errors
    /// Returns [`LayoutCacheError::Cas`] on store I/O errors, or
    /// [`LayoutCacheError::Decode`] / [`LayoutCacheError::Encode`] on rkyv failure.
    ///
    /// # Panics
    /// This function does not panic in practice. The `expect("Infallible cannot fail")`
    /// call handles a `std::convert::Infallible` error arm that is statically impossible.
    pub fn get_or_build(&mut self, text: &str) -> Result<Vec<LayoutSpan>, LayoutCacheError> {
        let short_key = self.short_key(text);
        let cached_hash = self.key_index.lock().get(&short_key).copied();
        if let Some(h) = cached_hash {
            if let Some(bytes) = self.store.get(h)? {
                let archived = rkyv::check_archived_root::<Vec<LayoutSpan>>(&bytes)
                    .map_err(|e| LayoutCacheError::Decode(format!("{e:?}")))?;
                let out: Vec<LayoutSpan> = archived
                    .deserialize(&mut Infallible)
                    .expect("Infallible cannot fail");
                return Ok(out);
            }
        }
        let spans = self.build_spans(text);
        let bytes =
            rkyv::to_bytes::<_, 1024>(&spans).map_err(|e| LayoutCacheError::Encode(format!("{e:?}")))?;
        let h = self.store.put(&bytes)?;
        self.key_index.lock().insert(short_key, h);
        Ok(spans)
    }

    /// Derive a `u64` short-key that encodes `(viewport_cols, text)`.
    fn short_key(&self, text: &str) -> u64 {
        let mut h = blake3::Hasher::new();
        h.update(b"layout/v1/");
        h.update(&self.viewport_cols.to_be_bytes());
        h.update(text.as_bytes());
        let digest = h.finalize();
        fxhash::hash64(digest.as_bytes())
    }

    /// Build the wrapped [`LayoutSpan`] vector for `text`.
    fn build_spans(&mut self, text: &str) -> Vec<LayoutSpan> {
        let mut spans: Vec<LayoutSpan> = Vec::new();
        if text.is_empty() {
            return spans;
        }
        let mut cur_row: u16 = 0;
        let mut cur_col: u16 = 0;
        let mut cur_byte_start: u32 = 0;
        let mut byte_cursor: u32 = 0;
        let max_cols = self.viewport_cols.max(1);
        for g in text.graphemes(true) {
            let g_bytes = u32::try_from(g.len()).unwrap_or(u32::MAX);
            let w = u16::from(self.widths.width_of(g).max(1));
            if cur_col + w > max_cols {
                spans.push(LayoutSpan {
                    row: cur_row,
                    col: 0,
                    byte_start: cur_byte_start,
                    byte_end: byte_cursor,
                });
                cur_row = cur_row.saturating_add(1);
                cur_col = 0;
                cur_byte_start = byte_cursor;
            }
            cur_col += w;
            byte_cursor = byte_cursor.saturating_add(g_bytes);
        }
        if byte_cursor > cur_byte_start {
            spans.push(LayoutSpan {
                row: cur_row,
                col: 0,
                byte_start: cur_byte_start,
                byte_end: byte_cursor,
            });
        }
        spans
    }
}
