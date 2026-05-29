// SPDX-License-Identifier: Apache-2.0
//! Credit-budgeted MPSC channel (N7.4).
//!
//! A `CreditChannel<T>` pairs a `tokio::sync::Semaphore` (the credit pool)
//! with a `tokio::sync::mpsc::channel` (the message queue). The semaphore
//! starts at `budget` permits; every `try_send` consumes one synchronously
//! (returning `WouldBlock` if none are available), every `recv` releases one
//! back to the pool.
//!
//! The net effect: senders cannot get ahead of consumers by more than `budget`
//! messages without back-pressure, regardless of how the underlying mpsc
//! buffer is sized.

use std::sync::Arc;

use thiserror::Error;
use tokio::sync::{mpsc, Semaphore};

/// Errors returned by [`CreditSender::try_send`].
#[derive(Debug, Error)]
pub enum TrySendError {
    /// All `budget` credits are currently in flight; caller should retry once
    /// a consumer has called `recv`.
    #[error("would block: credit budget exhausted")]
    WouldBlock,
    /// Receiver has been dropped — the channel is permanently closed.
    #[error("channel closed")]
    Closed,
}

/// Producer half of a [`CreditChannel`].
///
/// Cloneable so multiple producers can share the same credit pool (the
/// pool guarantees the aggregate in-flight count never exceeds `budget`).
#[derive(Debug)]
pub struct CreditSender<T> {
    tx: mpsc::Sender<T>,
    permits: Arc<Semaphore>,
}

impl<T> Clone for CreditSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            permits: Arc::clone(&self.permits),
        }
    }
}

impl<T> CreditSender<T> {
    /// Send a value without awaiting. Consumes one credit from the pool
    /// **before** the mpsc enqueue; the credit is restored by the receiver on
    /// `recv` (so the sender unblocks once the consumer drains a slot).
    ///
    /// # Errors
    /// - [`TrySendError::WouldBlock`] if no credit is available.
    /// - [`TrySendError::Closed`] if the receiver has been dropped.
    pub fn try_send(&self, value: T) -> Result<(), TrySendError> {
        // `try_acquire` returns `Err` if no permits remain. Forget the
        // permit immediately so it stays consumed until the receiver issues
        // a `release()` on the underlying semaphore — there's no useful
        // RAII lifetime here, so we keep the value off the stack to satisfy
        // Clippy `significant_drop_tightening`.
        self.permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| TrySendError::WouldBlock)?
            .forget();
        match self.tx.try_send(value) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                // The mpsc buffer is sized to `budget`, so a credit-controlled
                // send can only see `Full` if a producer raced past the
                // permit/enqueue boundary — refund and retry semantics make
                // most sense here, but for the synchronous `try_send` API we
                // re-issue the permit and surface `WouldBlock` so the caller
                // is free to retry.
                self.permits.add_permits(1);
                Err(TrySendError::WouldBlock)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // No consumer to ever return the credit; surface `Closed`.
                Err(TrySendError::Closed)
            }
        }
    }
}

/// Consumer half of a [`CreditChannel`].
#[derive(Debug)]
pub struct CreditReceiver<T> {
    rx: mpsc::Receiver<T>,
    permits: Arc<Semaphore>,
}

impl<T> CreditReceiver<T> {
    /// Receive the next value, releasing one credit back to the pool on
    /// success. Returns `None` when every sender has been dropped and the
    /// queue is empty.
    pub async fn recv(&mut self) -> Option<T> {
        let v = self.rx.recv().await?;
        self.permits.add_permits(1);
        Some(v)
    }

    /// Non-blocking variant of [`Self::recv`].
    ///
    /// # Errors
    /// Returns the standard `tokio::sync::mpsc::error::TryRecvError` so
    /// callers can distinguish `Empty` from `Disconnected`.
    pub fn try_recv(&mut self) -> Result<T, mpsc::error::TryRecvError> {
        let v = self.rx.try_recv()?;
        self.permits.add_permits(1);
        Ok(v)
    }
}

/// Generic credit-budgeted channel.
///
/// Construct one via [`CreditChannel::new`] which returns the matched
/// `(sender, receiver)` pair sharing a single semaphore.
pub struct CreditChannel<T>(std::marker::PhantomData<T>);

impl<T> CreditChannel<T> {
    /// Create a channel with `budget` credits and an internal mpsc buffer of
    /// the same size.
    ///
    /// `budget` of zero is technically legal — every send will immediately
    /// surface `WouldBlock` and consumers can never make progress without
    /// outside intervention — but is almost certainly a caller bug. The
    /// underlying `mpsc::channel` requires a non-zero capacity, so we clamp
    /// the buffer to `max(budget, 1)`.
    #[must_use]
    // `new` returning a `(sender, receiver)` pair is the canonical Rust
    // channel idiom (see `mpsc::channel`, `oneshot::channel`); Clippy's
    // `new_ret_no_self` lint asks for `Self` here, which we deliberately
    // don't want.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(budget: u32) -> (CreditSender<T>, CreditReceiver<T>) {
        let buf = usize::try_from(budget.max(1)).unwrap_or(usize::MAX);
        let (tx, rx) = mpsc::channel(buf);
        let permits = Arc::new(Semaphore::new(usize::try_from(budget).unwrap_or(0)));
        (
            CreditSender {
                tx,
                permits: Arc::clone(&permits),
            },
            CreditReceiver { rx, permits },
        )
    }
}
