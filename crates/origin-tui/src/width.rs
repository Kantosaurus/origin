//! Grapheme-width LRU cache (N8.4).

use lru::LruCache;
use std::num::NonZeroUsize;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[allow(clippy::module_name_repetitions)]
#[derive(Debug)]
pub struct WidthCache {
    map: LruCache<u64, u8>,
}

impl WidthCache {
    /// # Panics
    /// Panics if `cap == 0`.
    #[must_use]
    pub fn new(cap: usize) -> Self {
        let nz = NonZeroUsize::new(cap).expect("WidthCache capacity must be > 0");
        Self {
            map: LruCache::new(nz),
        }
    }

    pub fn width_of(&mut self, grapheme: &str) -> u8 {
        let key = fxhash::hash64(grapheme.as_bytes());
        if let Some(&w) = self.map.get(&key) {
            return w;
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "capped to 2 by `.min(2)` before cast"
        )]
        let w = UnicodeWidthStr::width(grapheme).min(2) as u8;
        self.map.put(key, w);
        w
    }

    pub fn measure_str(&mut self, text: &str) -> u32 {
        text.graphemes(true).map(|g| u32::from(self.width_of(g))).sum()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}
