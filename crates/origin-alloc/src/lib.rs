//! `origin-alloc` — per-component allocator arenas with a no-op default and an
//! opt-in jemalloc backend.

pub mod arena_id;
pub mod scope;

#[cfg(not(feature = "jemalloc"))]
mod noop_backend;
#[cfg(not(feature = "jemalloc"))]
pub(crate) use noop_backend as backend;

#[cfg(feature = "jemalloc")]
mod jemalloc_backend;
#[cfg(feature = "jemalloc")]
pub(crate) use jemalloc_backend as backend;

pub use arena_id::ArenaId;
pub use scope::ArenaScope;

/// Re-export of the jemalloc allocator type so binaries that link this crate
/// (with the `jemalloc` feature) can opt in as the global allocator:
///
/// ```ignore
/// #[global_allocator]
/// static GLOBAL: origin_alloc::JemallocAllocator = origin_alloc::JemallocAllocator;
/// ```
///
/// The library itself does NOT install a `#[global_allocator]` — that is the
/// binary's choice. Per-arena MALLCTL calls still function regardless because
/// `tikv-jemalloc-sys` links the jemalloc symbols in unconditionally.
#[cfg(feature = "jemalloc")]
pub use tikv_jemallocator::Jemalloc as JemallocAllocator;

use thiserror::Error;

/// Per-arena resident / allocated byte snapshot.
#[cfg(feature = "jemalloc")]
pub use crate::jemalloc_backend::ArenaStat;
/// Per-arena resident / allocated byte snapshot (no-op backend — all zeros).
#[cfg(not(feature = "jemalloc"))]
pub use crate::noop_backend::ArenaStat;

#[derive(Debug, Error)]
pub enum AllocError {
    #[error("backend rejected arena bind for `{0:?}`: {1}")]
    Bind(ArenaId, String),
    #[error("backend not available")]
    Unavailable,
}

/// Enter a scope bound to `id`. The closure runs synchronously; allocations
/// inside it are attributed to the arena. The scope is restored on return.
///
/// # Errors
/// Returns [`AllocError::Bind`] if the backend rejects the bind (jemalloc only).
pub fn with_arena<R>(id: ArenaId, f: impl FnOnce(&ArenaScope) -> R) -> Result<R, AllocError> {
    let prev = backend::bind_thread_arena(id);
    let scope = ArenaScope::new(id, prev);
    let out = f(&scope);
    drop(scope); // Drop restores `prev`.
    Ok(out)
}

/// Snapshot of resident bytes per arena. No-op backend returns all zeros.
///
/// # Errors
/// Returns [`AllocError::Unavailable`] on the no-op backend (currently never —
/// the no-op snapshot always succeeds and returns zeros).
pub fn stats_snapshot() -> Result<[backend::ArenaStat; ArenaId::COUNT], AllocError> {
    backend::snapshot()
}

/// `arena.<i>.reset` — drop physical pages without invalidating the arena.
///
/// # Errors
/// Returns [`AllocError::Bind`] if the underlying `mallctl` fails;
/// [`AllocError::Unavailable`] on the no-op backend.
// jemalloc backend's `reset_arena` is non-const (FFI); keep this non-const for
// API parity across backends.
#[allow(clippy::missing_const_for_fn)]
pub fn reset(id: ArenaId) -> Result<(), AllocError> {
    backend::reset_arena(id)
}

/// `arena.<i>.destroy` — fully invalidate the arena. Subsequent `with_arena`
/// for the same id allocates a fresh jemalloc arena.
///
/// # Errors
/// Returns [`AllocError::Bind`] if the underlying `mallctl` fails;
/// [`AllocError::Unavailable`] on the no-op backend.
// jemalloc backend's `destroy_arena` is non-const (FFI); keep this non-const
// for API parity across backends.
#[allow(clippy::missing_const_for_fn)]
pub fn destroy(id: ArenaId) -> Result<(), AllocError> {
    backend::destroy_arena(id)
}
