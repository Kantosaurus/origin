//! Process-singleton runtime registry — per-class semaphores + bulk gate.

use crate::class::TaskClass;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::sync::{Notify, Semaphore};

pub(crate) struct Registry {
    pub(crate) sema: [Arc<Semaphore>; TaskClass::COUNT],
    pub(crate) bulk_gate: Arc<Notify>,
    pub(crate) critical_busy: AtomicI32,
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();
static INIT_LOCK: Mutex<()> = Mutex::new(());

fn permits_for(class: TaskClass) -> usize {
    let cores = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(4);
    match class {
        TaskClass::Critical => (cores * 4).max(8),
        TaskClass::Realtime => (cores * 2).max(4),
        TaskClass::Sidecar => cores.max(2),
        TaskClass::Background => 2,
        TaskClass::Bulk => 1,
    }
}

pub(crate) fn registry() -> &'static Registry {
    REGISTRY.get_or_init(|| {
        let _g = INIT_LOCK.lock();
        Registry {
            sema: [
                Arc::new(Semaphore::new(permits_for(TaskClass::Critical))),
                Arc::new(Semaphore::new(permits_for(TaskClass::Realtime))),
                Arc::new(Semaphore::new(permits_for(TaskClass::Sidecar))),
                Arc::new(Semaphore::new(permits_for(TaskClass::Background))),
                Arc::new(Semaphore::new(permits_for(TaskClass::Bulk))),
            ],
            bulk_gate: Arc::new(Notify::new()),
            critical_busy: AtomicI32::new(0),
        }
    })
}

/// Test helper — does nothing on the second call (registry is singleton).
pub fn init_for_test() {
    let _ = registry();
}

pub(crate) fn note_critical_acquire() {
    registry().critical_busy.fetch_add(1, Ordering::SeqCst);
}

pub(crate) fn note_critical_release() {
    let prev = registry().critical_busy.fetch_sub(1, Ordering::SeqCst);
    if prev <= 1 {
        // No more critical work — release any parked Bulk tasks.
        registry().bulk_gate.notify_waiters();
    }
}

pub(crate) fn critical_in_flight() -> i32 {
    registry().critical_busy.load(Ordering::SeqCst)
}
