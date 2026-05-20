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

use thiserror::Error;

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
pub fn with_arena<R>(
    id: ArenaId,
    f: impl FnOnce(&ArenaScope) -> R,
) -> Result<R, AllocError> {
    let prev = backend::bind_thread_arena(id);
    let scope = ArenaScope::new(id, prev);
    let out = f(&scope);
    drop(scope); // Drop restores `prev`.
    Ok(out)
}
