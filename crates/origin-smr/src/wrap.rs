//! Wrap-around byte copy helpers for the ring's payload region.
//!
//! Factored out of `ring.rs` so the framing logic there stays under
//! the 300-LOC budget. All three helpers are `unsafe` because they
//! take raw pointers into the mapping; SAFETY comments on every
//! `unsafe { ... }` inside describe what holds at the call sites.

use crate::cursor::PAYLOAD_OFFSET;

/// Copy `src` into the payload region starting at offset `start_off`
/// (relative to the start of the payload, i.e. 0..usable), wrapping at
/// `usable`.
///
/// # Safety
/// `base` must point to a writable mapping of at least
/// `PAYLOAD_OFFSET + usable` bytes; `start_off + src.len()` may exceed
/// `usable` (we wrap), but `src.len() <= usable` must hold so the two
/// segments don't overlap.
pub unsafe fn wrap_copy_in(base: *mut u8, start_off: usize, usable: usize, src: &[u8]) {
    debug_assert!(src.len() <= usable);
    let first = core::cmp::min(usable - start_off, src.len());
    // SAFETY: `start_off < usable` by `%` upstream; `first` is the
    // smaller of "bytes until wrap" and "bytes to copy"; the
    // destination range stays inside `[PAYLOAD_OFFSET,
    // PAYLOAD_OFFSET + usable)`.
    unsafe {
        core::ptr::copy_nonoverlapping(src.as_ptr(), base.add(PAYLOAD_OFFSET + start_off), first);
    }
    let remaining = src.len() - first;
    if remaining > 0 {
        // SAFETY: `remaining <= usable` because the total source slice
        // is at most `usable` bytes (caller invariant); writing it at
        // offset 0 of the payload is in-bounds.
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr().add(first), base.add(PAYLOAD_OFFSET), remaining);
        }
    }
}

/// Mirror of `wrap_copy_in` for the consumer side.
///
/// # Safety
/// Same invariants as `wrap_copy_in`: `base` is a valid mapping of at
/// least `PAYLOAD_OFFSET + usable` bytes, and `dst.len() <= usable`.
pub unsafe fn wrap_copy_out(base: *const u8, start_off: usize, usable: usize, dst: &mut [u8]) {
    debug_assert!(dst.len() <= usable);
    let first = core::cmp::min(usable - start_off, dst.len());
    // SAFETY: identical to the producer-side reasoning, but for a
    // read instead of a write; both segments stay in-bounds.
    unsafe {
        core::ptr::copy_nonoverlapping(base.add(PAYLOAD_OFFSET + start_off), dst.as_mut_ptr(), first);
    }
    let remaining = dst.len() - first;
    if remaining > 0 {
        // SAFETY: wrapped tail of the read; offset 0 of payload is
        // in-bounds and `remaining <= usable`.
        unsafe {
            core::ptr::copy_nonoverlapping(base.add(PAYLOAD_OFFSET), dst.as_mut_ptr().add(first), remaining);
        }
    }
}

/// Helper for backends: zero the cursor pair at the start of a freshly
/// created mapping.
///
/// # Safety
/// `base` must point to a writable mapping of at least `PAYLOAD_OFFSET`
/// bytes.
pub const unsafe fn zero_cursors(base: *mut u8) {
    // SAFETY: caller guarantees `base..base + PAYLOAD_OFFSET` is a
    // valid writable mapping. `write_bytes` is the canonical way to
    // memset through a raw pointer.
    unsafe { core::ptr::write_bytes(base, 0u8, PAYLOAD_OFFSET) };
}
