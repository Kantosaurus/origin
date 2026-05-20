//! RAII guard that pins the current thread to an `ArenaId` for the duration of
//! a closure. Re-entrant: nested scopes restore the previous binding on drop.

use crate::arena_id::ArenaId;

/// RAII binding of the current thread to an `ArenaId`.
#[must_use = "the scope must outlive any allocations attributed to it"]
#[allow(clippy::module_name_repetitions)]
pub struct ArenaScope {
    id: ArenaId,
    // Restoration of the prior thread-arena binding is the backend's job; this
    // field is private and lives only for `Drop`. Stored as `Option<u32>` to
    // match the jemalloc backend's native arena index type.
    pub(crate) prev_index: Option<u32>,
}

impl ArenaScope {
    /// Arena this scope is currently bound to.
    #[must_use]
    pub const fn id(&self) -> ArenaId {
        self.id
    }

    pub(crate) const fn new(id: ArenaId, prev_index: Option<u32>) -> Self {
        Self { id, prev_index }
    }
}

impl Drop for ArenaScope {
    fn drop(&mut self) {
        crate::backend::restore_thread_arena(self.prev_index);
    }
}
