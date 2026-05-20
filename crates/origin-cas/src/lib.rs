//! `origin-cas` — content-addressed store.
//!
//! Phase 2 deliverables: Hash, FastCDC chunker, mmap pack files, three-tier
//! Store, refcount + GC.

#![deny(clippy::undocumented_unsafe_blocks)]

pub mod chunker;
pub mod dict;
mod hash;
mod packfile;
#[cfg(all(target_os = "linux", feature = "uring"))]
pub mod packfile_uring;
mod refs;
mod store;

pub use chunker::{chunks, ChunkIter, ChunkRef};
pub use dict::{DictError, DictVersion};
pub use hash::Hash;
pub use packfile::{IndexEntry, PackBuilder, PackError, PackReader, PackSlice};
pub use refs::{RefError, RefTable};
pub use store::{Store, StoreConfig, StoreError};

#[cfg(feature = "recorder")]
pub mod recorder_hook {
    use origin_replay::cas_tap::CasTap;
    use std::sync::Arc;

    static TAP: parking_lot::RwLock<Option<Arc<CasTap>>> = parking_lot::RwLock::new(None);

    pub fn register_tap(tap: Arc<CasTap>) {
        *TAP.write() = Some(tap);
    }

    #[must_use]
    pub fn tap() -> Option<Arc<CasTap>> {
        TAP.read().clone()
    }
}
