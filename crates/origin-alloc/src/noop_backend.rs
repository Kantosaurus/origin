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
