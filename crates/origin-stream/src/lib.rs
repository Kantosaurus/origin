//! `origin-stream` — single-producer multi-consumer byte ring.
//!
//! Mechanism N2.1: one append-only `Bytes` buffer + an atomic write cursor;
//! each subscriber holds its own read cursor. Wakeups via `tokio::sync::Notify`.
//! After warmup the ring never reallocates (it's a fixed-capacity buffer).
//!
//! Records are rkyv-archived `TokenEvent`s, length-prefixed (`u32` BE).

#![deny(clippy::undocumented_unsafe_blocks)]

mod event;

pub use event::{TokenEvent, TokenKind};

use parking_lot::Mutex;
use rkyv::{check_archived_root, Deserialize, Infallible};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Notify;

#[derive(Debug, Error)]
pub enum RingError {
    #[error("ring closed")]
    Closed,
    #[error("record too large for ring: {0} bytes")]
    TooLarge(usize),
    #[error("rkyv encode: {0}")]
    Encode(String),
    #[error("rkyv decode: {0}")]
    Decode(String),
}

struct Inner {
    buf: Mutex<Vec<u8>>,
    write_cursor: AtomicUsize,
    notify: Notify,
    closed: AtomicBool,
    capacity: usize,
}

/// Cloneable handle to the underlying ring.
#[derive(Clone)]
pub struct Ring {
    inner: Arc<Inner>,
}

impl Ring {
    /// Create a ring with a fixed byte capacity. Records exceeding capacity
    /// fail with `TooLarge`.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                buf: Mutex::new(Vec::with_capacity(capacity)),
                write_cursor: AtomicUsize::new(0),
                notify: Notify::new(),
                closed: AtomicBool::new(false),
                capacity,
            }),
        }
    }

    /// Append a `TokenEvent`. Wakes all subscribers.
    ///
    /// # Errors
    /// `Closed` if the producer has called `close()`; `TooLarge` if the
    /// archived record + length prefix don't fit the remaining capacity.
    /// (Phase 2: no wrap-around. The ring is sized for one turn.)
    pub fn publish(&self, ev: &TokenEvent) -> Result<(), RingError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(RingError::Closed);
        }
        let bytes = rkyv::to_bytes::<_, 256>(ev).map_err(|e| RingError::Encode(e.to_string()))?;
        let len = u32::try_from(bytes.len()).map_err(|_| RingError::TooLarge(bytes.len()))?;

        let mut buf = self.inner.buf.lock();
        let new_total = buf.len() + 4 + bytes.len();
        if new_total > self.inner.capacity {
            return Err(RingError::TooLarge(bytes.len()));
        }
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&bytes);
        self.inner.write_cursor.store(buf.len(), Ordering::Release);
        drop(buf);
        self.inner.notify.notify_waiters();
        Ok(())
    }

    /// Mark the ring as closed; subscribers see `Ok(None)` after the last record.
    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::Release);
        self.inner.notify.notify_waiters();
    }

    /// Create a subscriber starting at the current write cursor.
    #[must_use]
    pub fn subscribe(&self) -> Subscriber {
        let start = self.inner.write_cursor.load(Ordering::Acquire);
        Subscriber {
            ring: self.clone(),
            read_cursor: start,
        }
    }
}

/// Panic-free length-prefixed rkyv-archived `TokenEvent` decoder for fuzz
/// targets.
///
/// Walks the input as a sequence of `u32` BE length-prefixed records and
/// validates each via `check_archived_root::<TokenEvent>`. This is the
/// same decode path used by ring subscribers and MUST NOT panic on
/// arbitrary input.
///
/// # Errors
/// Returns `RingError::Decode` on the first malformed record (truncated
/// length prefix, length exceeding remaining bytes, or rkyv validation
/// failure). Returns `Ok(())` on an empty input or a clean walk.
pub fn parse(bytes: &[u8]) -> Result<(), RingError> {
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if cursor.checked_add(4).map_or(true, |end| end > bytes.len()) {
            return Err(RingError::Decode("len prefix truncated".into()));
        }
        let len_bytes: [u8; 4] = bytes[cursor..cursor + 4]
            .try_into()
            .map_err(|_| RingError::Decode("len prefix".into()))?;
        let len = u32::from_be_bytes(len_bytes) as usize;
        let start = cursor + 4;
        let end = start
            .checked_add(len)
            .ok_or_else(|| RingError::Decode("len overflow".into()))?;
        if end > bytes.len() {
            return Err(RingError::Decode("record truncated".into()));
        }
        let slice = &bytes[start..end];
        check_archived_root::<TokenEvent>(slice).map_err(|e| RingError::Decode(format!("{e:?}")))?;
        cursor = end;
    }
    Ok(())
}

/// One tail. Each subscriber tracks its own read position.
pub struct Subscriber {
    ring: Ring,
    read_cursor: usize,
}

impl Subscriber {
    /// Await the next `TokenEvent`. Returns `Ok(None)` when the ring closes
    /// and the subscriber has drained all records.
    ///
    /// # Errors
    /// Propagates rkyv decode errors.
    pub async fn next(&mut self) -> Result<Option<TokenEvent>, RingError> {
        loop {
            let write = self.ring.inner.write_cursor.load(Ordering::Acquire);
            if self.read_cursor < write {
                let buf = self.ring.inner.buf.lock();
                let len_bytes: [u8; 4] = buf[self.read_cursor..self.read_cursor + 4]
                    .try_into()
                    .map_err(|_| RingError::Decode("len prefix".into()))?;
                let len = u32::from_be_bytes(len_bytes) as usize;
                let start = self.read_cursor + 4;
                let end = start + len;
                let slice = &buf[start..end];
                let archived = check_archived_root::<TokenEvent>(slice)
                    .map_err(|e| RingError::Decode(format!("{e:?}")))?;
                let ev: TokenEvent = archived
                    .deserialize(&mut Infallible)
                    .map_err(|e| RingError::Decode(format!("{e:?}")))?;
                self.read_cursor = end;
                return Ok(Some(ev));
            }
            if self.ring.inner.closed.load(Ordering::Acquire) {
                // Re-check the write cursor before declaring end-of-stream: a
                // record may have been published immediately before the close.
                // The Acquire load of `closed` synchronizes-with the producer's
                // Release stores, so a fresh write_cursor load here observes any
                // record written before the close. The initial check at the top
                // of the loop could have read a stale (pre-publish) cursor.
                if self.ring.inner.write_cursor.load(Ordering::Acquire) > self.read_cursor {
                    continue;
                }
                return Ok(None);
            }
            let notified = self.ring.inner.notify.notified();
            // Re-check under the notified future to close the wake-race window.
            if self.ring.inner.write_cursor.load(Ordering::Acquire) > self.read_cursor
                || self.ring.inner.closed.load(Ordering::Acquire)
            {
                continue;
            }
            notified.await;
        }
    }
}
