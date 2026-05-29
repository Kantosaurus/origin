// SPDX-License-Identifier: Apache-2.0
//! SPSC byte ring + rkyv framing.
//!
//! Frame layout: `[u32 little-endian length][rkyv bytes]`. The 4-byte
//! length prefix is itself part of the wrap-around accounting; we copy
//! it through the same `wrap_copy_in` / `wrap_copy_out` helper as the
//! payload so the math is one expression rather than two.
//!
//! Ordering: producer publishes `head` with `Release` after writing,
//! consumer publishes `tail` with `Release` after reading. The opposite
//! side reads with `Acquire`. Length-prefix and payload bytes therefore
//! happen-before the new cursor value the other side observes.

use core::sync::atomic::Ordering;

use rkyv::{check_archived_root, AlignedVec, Deserialize};

use crate::cursor::{Cursors, PAYLOAD_OFFSET};
use crate::event::SwarmEvent;
use crate::wrap::{wrap_copy_in, wrap_copy_out};
use crate::{RingError, TrySendError};

/// Minimum mapping size — must be large enough to hold both cursors
/// plus a single moderate event. 4 KiB matches the page size on every
/// target we ship to and is what the test suite uses.
pub const MIN_CAPACITY: usize = 4096;

/// Length-prefix size (bytes). We use `u32` so a single event maxes out
/// at ~4 GiB which is well beyond anything we'd put in a 4 KiB ring.
pub const LEN_PREFIX_BYTES: usize = 4;

/// Owned mapping handle returned by the backend.
///
/// We split this from `Ring` so the unsafe mmap lifetime sits in one
/// place and the SPSC framing logic can be backend-agnostic.
pub struct Mapping {
    /// Pointer to the first byte of the mapping.
    pub ptr: *mut u8,
    /// Total mapping size in bytes (cursors + payload).
    pub capacity: usize,
    /// True iff this side created the shm object (and therefore must
    /// unlink it on drop).
    pub owns_name: bool,
    /// POSIX shm path (`/origin-smr-…`) on Unix; UTF-16-encoded handle
    /// name on Windows. Kept so `Drop` can unlink/close.
    pub raw_name: String,
    /// Native handle stash for the Windows backend (file mapping
    /// `HANDLE`). Unix backends ignore this field.
    pub native_handle: isize,
}

// SAFETY: a `Mapping` only owns a raw pointer into an OS-managed shared
// mapping. The pointer is never dereferenced concurrently from multiple
// threads without going through the SPSC discipline (head=producer,
// tail=consumer). Sending across thread boundaries is fine; sharing is
// also fine because all reads/writes funnel through atomics + acquire /
// release ordering on `head` and `tail`.
unsafe impl Send for Mapping {}
// SAFETY: see `Send` above. SPSC discipline + acquire/release on the
// cursors prevents data races; the ring itself enforces single-producer
// / single-consumer by contract (callers must not call `try_send` from
// two threads concurrently).
unsafe impl Sync for Mapping {}

/// User-facing handle. Wraps a `Mapping` and provides `try_send` /
/// `try_recv`. Drop semantics are delegated to the backend module.
pub struct Ring {
    pub(crate) mapping: Mapping,
}

impl Ring {
    /// Usable payload bytes (capacity minus the two cursor cache lines).
    #[must_use]
    pub const fn usable(&self) -> usize {
        self.mapping.capacity - PAYLOAD_OFFSET
    }

    /// Single-producer send. Serializes the event with rkyv, writes a
    /// 4-byte LE length prefix followed by the bytes, and publishes
    /// the new `head` with `Release`. Returns `WouldBlock` if the ring
    /// is too full to accept the frame right now.
    ///
    /// # Errors
    /// `TrySendError::WouldBlock` when free space < needed.
    /// `TrySendError::Ring(CapacityExceeded)` if a single event is
    /// larger than the entire usable area (the ring can never accept
    /// it — this is a permanent error, not back-pressure).
    pub fn try_send(&self, event: &SwarmEvent) -> Result<(), TrySendError> {
        let serialized = rkyv::to_bytes::<_, 256>(event)
            .map_err(|e| TrySendError::Ring(RingError::ValidationFailed(format!("rkyv: {e}"))))?;
        // Copy into a plain `Vec` so the byte-by-byte ring write does
        // not depend on rkyv's `AlignedVec` ABI.
        let bytes: Vec<u8> = serialized.as_slice().to_vec();
        let needed = LEN_PREFIX_BYTES + bytes.len();
        if needed > self.usable() {
            return Err(TrySendError::Ring(RingError::CapacityExceeded {
                needed,
                capacity: self.usable(),
            }));
        }

        let cursors;
        // SAFETY: `mapping.ptr` is the base of a valid writable mapping
        // of `mapping.capacity` bytes (upheld by backend open).
        unsafe {
            cursors = Cursors::from_base(self.mapping.ptr);
        }
        let head = cursors.head.load(Ordering::Acquire);
        let tail = cursors.tail.load(Ordering::Acquire);
        let in_flight = usize::try_from(head.wrapping_sub(tail)).map_err(|_| {
            TrySendError::Ring(RingError::ValidationFailed(
                "in-flight bytes exceeds usize".into(),
            ))
        })?;
        if in_flight + needed > self.usable() {
            return Err(TrySendError::WouldBlock);
        }

        let usable = self.usable();
        let usable_u64 = u64::try_from(usable)
            .map_err(|_| TrySendError::Ring(RingError::ValidationFailed("usable exceeds u64".into())))?;
        let write_off = usize::try_from(head % usable_u64)
            .map_err(|_| TrySendError::Ring(RingError::ValidationFailed("head offset overflow".into())))?;
        let len_prefix = u32::try_from(bytes.len())
            .map_err(|_| TrySendError::Ring(RingError::ValidationFailed("len > u32".into())))?
            .to_le_bytes();

        // SAFETY: payload region starts at `PAYLOAD_OFFSET` and is
        // `usable` bytes long. `wrap_copy_in` clamps every write to
        // `[PAYLOAD_OFFSET, PAYLOAD_OFFSET + usable)` and the length
        // check above guarantees `needed <= usable`. Single-producer
        // discipline ensures no other writer is touching the same
        // payload bytes concurrently.
        unsafe {
            wrap_copy_in(self.mapping.ptr, write_off, usable, &len_prefix);
            wrap_copy_in(
                self.mapping.ptr,
                (write_off + LEN_PREFIX_BYTES) % usable,
                usable,
                &bytes,
            );
        }

        let needed_u64 = u64::try_from(needed)
            .map_err(|_| TrySendError::Ring(RingError::ValidationFailed("needed > u64".into())))?;
        cursors
            .head
            .store(head.wrapping_add(needed_u64), Ordering::Release);
        Ok(())
    }

    /// Single-consumer recv. Returns `Ok(None)` when the ring is empty.
    ///
    /// # Errors
    /// `RingError::ValidationFailed` if rkyv validation fails on the
    /// archived bytes — this means a producer wrote a corrupt frame or
    /// the framing is out of sync, both of which are bugs.
    pub fn try_recv(&self) -> Result<Option<SwarmEvent>, RingError> {
        let cursors;
        // SAFETY: same invariants as the producer path — `mapping.ptr`
        // is a valid mapping of `mapping.capacity` bytes.
        unsafe {
            cursors = Cursors::from_base(self.mapping.ptr);
        }
        let head = cursors.head.load(Ordering::Acquire);
        let tail = cursors.tail.load(Ordering::Acquire);
        if head == tail {
            return Ok(None);
        }

        let usable = self.usable();
        let usable_u64 =
            u64::try_from(usable).map_err(|_| RingError::ValidationFailed("usable exceeds u64".into()))?;
        let read_off = usize::try_from(tail % usable_u64)
            .map_err(|_| RingError::ValidationFailed("tail offset overflow".into()))?;
        let mut len_buf = [0u8; LEN_PREFIX_BYTES];
        // SAFETY: payload region is `usable` bytes; `wrap_copy_out`
        // clamps reads to that region; `head != tail` plus the
        // producer's release ordering guarantees the bytes are
        // initialized.
        unsafe { wrap_copy_out(self.mapping.ptr, read_off, usable, &mut len_buf) };
        let payload_len = u32::from_le_bytes(len_buf) as usize;
        if payload_len + LEN_PREFIX_BYTES > usable {
            return Err(RingError::ValidationFailed(format!(
                "frame len {payload_len} exceeds usable {usable}"
            )));
        }

        // rkyv requires its input to be 16-byte aligned, so we copy
        // into an `AlignedVec` before validation.
        let mut aligned = AlignedVec::with_capacity(payload_len);
        aligned.resize(payload_len, 0u8);
        // SAFETY: `aligned` was just sized to exactly `payload_len`
        // bytes; the wrap-aware copy clamps reads to the payload area.
        unsafe {
            wrap_copy_out(
                self.mapping.ptr,
                (read_off + LEN_PREFIX_BYTES) % usable,
                usable,
                aligned.as_mut_slice(),
            );
        }

        let archived = check_archived_root::<SwarmEvent>(&aligned)
            .map_err(|e| RingError::ValidationFailed(format!("rkyv check: {e}")))?;
        let event: SwarmEvent = Deserialize::<SwarmEvent, _>::deserialize(archived, &mut rkyv::Infallible)
            .map_err(|e| RingError::ValidationFailed(format!("rkyv deserialize: {e:?}")))?;

        let consumed = LEN_PREFIX_BYTES + payload_len;
        let consumed_u64 = u64::try_from(consumed)
            .map_err(|_| RingError::ValidationFailed("consumed exceeds u64".into()))?;
        cursors
            .tail
            .store(tail.wrapping_add(consumed_u64), Ordering::Release);
        Ok(Some(event))
    }

    /// Busy-spin send. Phase-13 may replace this with a futex/eventfd
    /// strategy; today it's a simple `spin_loop()` plus an optional
    /// `std::thread::yield_now()` after the first half of the
    /// deadline elapses (no Tokio dep needed at this layer).
    ///
    /// # Errors
    /// Propagates `RingError` from `try_send` on permanent errors;
    /// returns `RingError::ValidationFailed("send deadline expired")`
    /// once the deadline is reached.
    pub fn wait_send(&self, event: &SwarmEvent, deadline_ns: u64) -> Result<(), RingError> {
        let start = std::time::Instant::now();
        let half = std::time::Duration::from_nanos(deadline_ns / 2);
        let full = std::time::Duration::from_nanos(deadline_ns);
        loop {
            match self.try_send(event) {
                Ok(()) => return Ok(()),
                Err(TrySendError::WouldBlock) => {
                    let elapsed = start.elapsed();
                    if elapsed >= full {
                        return Err(RingError::ValidationFailed("send deadline expired".into()));
                    }
                    if elapsed >= half {
                        std::thread::yield_now();
                    } else {
                        core::hint::spin_loop();
                    }
                }
                Err(TrySendError::Ring(e)) => return Err(e),
            }
        }
    }
}
