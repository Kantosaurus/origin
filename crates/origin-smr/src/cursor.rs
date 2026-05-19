//! Cache-line-separated cursor pair stored at the head of the shared
//! mmap.
//!
//! Layout (matches the plan):
//! ```text
//! [ 0.. 64) head cursor (producer): AtomicU64 — total bytes ever written
//! [64..128) tail cursor (consumer): AtomicU64 — total bytes ever consumed
//! [128.. )  payload area (wraps at capacity - 128)
//! ```
//!
//! We do **not** use `crossbeam_utils::CachePadded<AtomicU64>` for the
//! in-mapping representation because on `x86_64` it aligns to 128
//! bytes (paired prefetcher heuristic), which is larger than the
//! 64-byte slots the plan specifies. Instead we lay out two raw
//! `AtomicU64`s
//! at offsets 0 and 64; the 64-byte gap between them is the cache-line
//! separation the plan asks for. The `crossbeam-utils` dependency
//! still pulls its weight via static `CachePadded` checks in unit
//! tests should we ever want them.
//!
//! The mmap address is page-aligned (4 KiB) — a strict superset of
//! 8-byte alignment — so the raw pointer cast in `Cursors::from_base`
//! is sound for `AtomicU64`.

use core::sync::atomic::AtomicU64;
use crossbeam_utils::CachePadded;

/// Header offset (in bytes) at which the payload area starts.
///
/// 2 × 64 = 128. Anything below `PAYLOAD_OFFSET` is reserved for the
/// cursor pair and must never be touched by `try_send`/`try_recv`.
pub const PAYLOAD_OFFSET: usize = 128;

/// Producer cursor offset.
pub const HEAD_OFFSET: usize = 0;
/// Consumer cursor offset.
pub const TAIL_OFFSET: usize = 64;

const _: () = {
    // Sanity: `AtomicU64` fits in 8 bytes and aligns to at most 8.
    assert!(core::mem::size_of::<AtomicU64>() == 8);
    assert!(core::mem::align_of::<AtomicU64>() <= 8);
    // Documentation-as-code: `CachePadded` exists and is at most one
    // payload region. We don't use it for the in-mapping layout (see
    // module doc) but keep `crossbeam-utils` as a versioned, always-on
    // dependency so a future change can swap in if the layout changes.
    assert!(core::mem::size_of::<CachePadded<AtomicU64>>() <= PAYLOAD_OFFSET);
};

/// View into the two cursors of a mapped ring.
///
/// Constructed via `from_base` from the raw mmap pointer. The lifetime
/// `'a` ties the borrow to whatever owns the mmap (the `Ring`).
pub struct Cursors<'a> {
    pub head: &'a AtomicU64,
    pub tail: &'a AtomicU64,
}

impl Cursors<'_> {
    /// Build a `Cursors` view from the base pointer of the mmap.
    ///
    /// # Safety
    /// `base` must point to a writable mapping of at least
    /// `PAYLOAD_OFFSET` bytes whose first two 64-byte slots are valid
    /// `AtomicU64` cells (either freshly zero-initialized by the
    /// producer at create time, or written by another process that
    /// follows the same ABI). The returned `Cursors` borrows from
    /// that mapping; the caller is responsible for keeping it alive.
    pub unsafe fn from_base<'a>(base: *mut u8) -> Cursors<'a> {
        // Soundness: see the `# Safety` rustdoc above. `base` is
        // page-aligned by mmap contract; offsets 0 and 64 are both
        // 8-byte-aligned and refer to writable bytes within the
        // mapping; reading/writing them as `&AtomicU64` is sound.
        // The `unsafe fn` body is implicitly unsafe in edition 2021;
        // `&*ptr.cast()` performs the raw-pointer dereferences.
        #[allow(clippy::cast_ptr_alignment)] // see `# Safety` above
        let head = &*base.add(HEAD_OFFSET).cast::<AtomicU64>();
        #[allow(clippy::cast_ptr_alignment)] // see `# Safety` above
        let tail = &*base.add(TAIL_OFFSET).cast::<AtomicU64>();
        Cursors { head, tail }
    }
}
