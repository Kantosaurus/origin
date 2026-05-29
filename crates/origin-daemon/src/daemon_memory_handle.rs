// SPDX-License-Identifier: Apache-2.0
//! `DaemonMemoryHandle` — re-exports the daemon's concrete [`MemoryHandle`].
//!
//! Implementation so callers can refer to it by the canonical name expected by
//! the P6.9 subsystem-B spec without importing from `memory_wiring` directly.
//!
//! The actual adapter logic lives in [`crate::memory_wiring::MemoryDispatchHandle`];
//! this module is a thin public alias so `LoopOptions` and `main.rs` use a
//! stable, intent-revealing name.

pub use crate::memory_wiring::MemoryDispatchHandle as DaemonMemoryHandle;
