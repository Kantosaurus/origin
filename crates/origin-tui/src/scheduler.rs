//! Frame coalescing scheduler (N8.2).
//!
//! `Handle::mark_dirty` flips an `AtomicBool` and notifies a `tokio::sync::
//! Notify`. `Scheduler::run` awaits the notify, sleeps until the next
//! `frame_budget`-aligned wake, then runs the render closure once. Multiple
//! dirty flips inside the budget coalesce into one render. Idle frames cost
//! zero — the task is parked on the notify.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::{sleep, Instant};

#[derive(Debug)]
pub struct Scheduler {
    inner: Arc<Inner>,
    frame_budget: Duration,
}

#[derive(Debug)]
struct Inner {
    dirty: AtomicBool,
    notify: Notify,
}

impl Scheduler {
    /// Construct a new scheduler with the given minimum interval between
    /// render frames. A typical value is `Duration::from_millis(6)` — the
    /// effective ≤166Hz cap from spec N8.2.
    #[must_use]
    pub fn new(frame_budget: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                dirty: AtomicBool::new(false),
                notify: Notify::new(),
            }),
            frame_budget,
        }
    }

    /// Lightweight clonable handle for `mark_dirty` callers.
    #[must_use]
    pub fn handle(&self) -> Handle {
        Handle {
            inner: self.inner.clone(),
        }
    }

    /// Drive the render loop. `render` is invoked at most once per
    /// `frame_budget` window, and only when the dirty flag has flipped.
    ///
    /// This is an infinite loop; the caller is expected to drop the task or
    /// abort it from outside (e.g., `tokio::spawn(...).abort()`).
    pub async fn run<F>(self, mut render: F)
    where
        F: FnMut() + Send,
    {
        let mut last_frame: Option<Instant> = None;
        loop {
            self.inner.notify.notified().await;
            if !self.inner.dirty.swap(false, Ordering::AcqRel) {
                continue;
            }
            if let Some(prev) = last_frame {
                let since = prev.elapsed();
                if since < self.frame_budget {
                    sleep(self.frame_budget - since).await;
                }
            }
            render();
            last_frame = Some(Instant::now());
        }
    }
}

/// Cheap clonable handle for setting the dirty flag.
#[derive(Clone, Debug)]
pub struct Handle {
    inner: Arc<Inner>,
}

impl Handle {
    /// Mark the screen dirty; wakes the scheduler if it was idle.
    pub fn mark_dirty(&self) {
        self.inner.dirty.store(true, Ordering::Release);
        self.inner.notify.notify_one();
    }
}
