// SPDX-License-Identifier: Apache-2.0
//! Frame coalescing scheduler (N8.2).

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

    #[must_use]
    pub fn handle(&self) -> Handle {
        Handle {
            inner: self.inner.clone(),
        }
    }

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
                // `checked_sub` is `Some` exactly while we are still inside the
                // frame budget (and never panics on underflow); `None` ⇒ the
                // budget already elapsed, so render immediately without sleeping.
                if let Some(remaining) = self.frame_budget.checked_sub(since) {
                    sleep(remaining).await;
                }
            }
            render();
            last_frame = Some(Instant::now());
        }
    }
}

#[derive(Clone, Debug)]
pub struct Handle {
    inner: Arc<Inner>,
}

impl Handle {
    pub fn mark_dirty(&self) {
        self.inner.dirty.store(true, Ordering::Release);
        self.inner.notify.notify_one();
    }
}
