//! `origin-smr` — Shared-Memory Ring (SPSC) for swarm fanout (N7.2).
//!
//! A single-producer / single-consumer byte ring backed by POSIX
//! shared memory on Unix and a page-file-backed file mapping on
//! Windows, framing rkyv-archived [`SwarmEvent`] records.
//!
//! The ring is intentionally low-level: no thread pool, no built-in
//! scheduling. It only provides the bytes-on-the-wire contract that
//! `origin-swarm` (P9.6) layers credit channels on top of.
//!
//! # Discipline
//! * **Single producer**: only one process / task may call
//!   [`Ring::try_send`] at a time. Multi-producer is unsupported
//!   (the cursor math assumes monotonic `head` from one writer).
//! * **Single consumer**: only one process / task may call
//!   [`Ring::try_recv`] at a time.
//! * **Capacity**: the mapping holds two 64-byte cache-line padded
//!   `AtomicU64` cursors followed by the payload. Usable bytes =
//!   `capacity_bytes - 128`. `capacity_bytes` must be at least 4096.
//!
//! # Quick example
//! ```no_run
//! use origin_smr::{Ring, RingConfig, SwarmEvent};
//!
//! let name = format!("origin-smr-doc-{}", std::process::id());
//! let producer = Ring::open(RingConfig {
//!     name: name.clone(),
//!     capacity_bytes: 4096,
//!     create: true,
//! })?;
//! let consumer = Ring::open(RingConfig {
//!     name,
//!     capacity_bytes: 4096,
//!     create: false,
//! })?;
//! producer.try_send(&SwarmEvent::Heartbeat { sender: [0; 16], now_ms: 1 })?;
//! let _evt = consumer.try_recv()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![allow(clippy::module_name_repetitions)]

use thiserror::Error;

pub mod cursor;
pub mod event;
pub mod ring;
mod wrap;

#[cfg(unix)]
mod backend_unix;
#[cfg(windows)]
mod backend_windows;

pub use event::{ArchivedSwarmEvent, SwarmEvent};
pub use ring::Ring;

/// Configuration handed to [`Ring::open`].
#[derive(Debug, Clone)]
pub struct RingConfig {
    /// Logical name of the shared region. Transformed to a POSIX shm
    /// path (`/`-prefixed) on Unix; passed through to
    /// `CreateFileMappingW` / `OpenFileMappingW` on Windows.
    pub name: String,
    /// Total mapping size in bytes (cursors + payload). Must be at
    /// least 4096.
    pub capacity_bytes: usize,
    /// `true` for the producer side (creates + zeroes the cursors).
    /// `false` for the consumer side (opens an existing region).
    pub create: bool,
}

/// Permanent failures of the ring itself (as distinct from transient
/// back-pressure expressed via [`TrySendError::WouldBlock`]).
#[derive(Debug, Error)]
pub enum RingError {
    /// `CreateFileMappingW` / `shm_open` / `ftruncate` failed.
    #[error("ring creation failed: {0}")]
    CreationFailed(String),

    /// `MapViewOfFile` / `mmap` failed.
    #[error("mmap failed: {0}")]
    MmapFailed(String),

    /// A single frame doesn't fit in the entire usable area; the ring
    /// can never accept it. This is a configuration bug, not
    /// back-pressure.
    #[error("frame too large: needed {needed}, capacity {capacity}")]
    CapacityExceeded { needed: usize, capacity: usize },

    /// rkyv validation failed on archived bytes, or framing got out
    /// of sync.
    #[error("validation failed: {0}")]
    ValidationFailed(String),
}

/// Distinguishes transient back-pressure from permanent ring errors.
#[derive(Debug, Error)]
pub enum TrySendError {
    /// Ring is full; caller should retry later (or use
    /// [`Ring::wait_send`]).
    #[error("would block: ring is full")]
    WouldBlock,

    /// A permanent error from the ring (validation, configuration,
    /// I/O).
    #[error("ring error: {0}")]
    Ring(#[from] RingError),
}

impl Ring {
    /// Open or create a shared-memory ring.
    ///
    /// # Errors
    /// Returns [`RingError`] if the backend FFI call fails or the
    /// requested capacity is below the minimum (4096 bytes).
    pub fn open(cfg: RingConfig) -> Result<Self, RingError> {
        #[cfg(unix)]
        {
            backend_unix::open(cfg)
        }
        #[cfg(windows)]
        {
            backend_windows::open(cfg)
        }
    }
}
