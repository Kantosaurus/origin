//! FastCDC content-defined chunker. ~16 KiB average chunk size.
//!
//! Why FastCDC: a small edit (one byte inserted) shifts only the chunk that
//! contains it; downstream chunks keep their content-defined boundaries and
//! hash to the same address. This is the basis of CAS dedup across turns.

use crate::Hash;

/// Single chunk emitted by the FastCDC iterator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkRef {
    pub offset: usize,
    pub length: usize,
    pub hash: Hash,
}

/// Average / min / max chunk sizes (bytes). Match Anthropic-typical tool-output
/// sizes; tweak in Phase 5 once we have shard-size telemetry.
const MIN_SIZE: u32 = 4 * 1024;
const AVG_SIZE: u32 = 16 * 1024;
const MAX_SIZE: u32 = 64 * 1024;

/// Iterate content-defined chunks over `data`.
#[must_use]
pub fn chunks(data: &[u8]) -> ChunkIter<'_> {
    ChunkIter {
        data,
        inner: fastcdc::v2020::FastCDC::new(data, MIN_SIZE, AVG_SIZE, MAX_SIZE),
    }
}

pub struct ChunkIter<'a> {
    data: &'a [u8],
    inner: fastcdc::v2020::FastCDC<'a>,
}

impl Iterator for ChunkIter<'_> {
    type Item = ChunkRef;

    fn next(&mut self) -> Option<Self::Item> {
        let c = self.inner.next()?;
        let slice = &self.data[c.offset..c.offset + c.length];
        Some(ChunkRef {
            offset: c.offset,
            length: c.length,
            hash: Hash::of(slice),
        })
    }
}
