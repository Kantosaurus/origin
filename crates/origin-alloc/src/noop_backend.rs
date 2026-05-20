//! No-op backend used when the `jemalloc` cargo feature is off. Every
//! `bind_thread_arena` is recorded for the routing test but no real allocator
//! state changes.

use crate::arena_id::ArenaId;
use std::cell::Cell;

thread_local! {
    static CURRENT: Cell<Option<usize>> = const { Cell::new(None) };
}

pub fn bind_thread_arena(id: ArenaId) -> Option<usize> {
    CURRENT.with(|c| c.replace(Some(id.backend_index())))
}

pub fn restore_thread_arena(prev: Option<usize>) {
    CURRENT.with(|c| c.set(prev));
}

#[must_use]
#[allow(dead_code)]
pub fn current_thread_arena() -> Option<usize> {
    CURRENT.with(Cell::get)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ArenaStat {
    pub resident_bytes: usize,
    pub allocated_bytes: usize,
    pub jemalloc_index: u32,
}

// The jemalloc backend returns `Result` (mallctl can fail); the no-op backend
// keeps the same signature for backend-trait parity.
#[allow(clippy::unnecessary_wraps)]
pub fn snapshot() -> Result<[ArenaStat; crate::arena_id::ArenaId::COUNT], super::AllocError> {
    Ok([ArenaStat::default(); crate::arena_id::ArenaId::COUNT])
}

pub const fn reset_arena(_id: crate::arena_id::ArenaId) -> Result<(), super::AllocError> {
    Err(super::AllocError::Unavailable)
}

pub const fn destroy_arena(_id: crate::arena_id::ArenaId) -> Result<(), super::AllocError> {
    Err(super::AllocError::Unavailable)
}
