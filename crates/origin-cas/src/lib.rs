//! `origin-cas` — content-addressed store.
//!
//! Phase 2 deliverables: Hash, FastCDC chunker, mmap pack files, three-tier
//! Store, refcount + GC.

#![deny(clippy::undocumented_unsafe_blocks)]

mod hash;

pub use hash::Hash;
