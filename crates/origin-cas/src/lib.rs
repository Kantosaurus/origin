//! `origin-cas` — content-addressed store.
//!
//! Phase 2 deliverables: Hash, FastCDC chunker, mmap pack files, three-tier
//! Store, refcount + GC.

#![deny(clippy::undocumented_unsafe_blocks)]

mod chunker;
mod hash;
mod packfile;

pub use chunker::{chunks, ChunkIter, ChunkRef};
pub use hash::Hash;
pub use packfile::{PackBuilder, PackError, PackReader, PackSlice};
