// SPDX-License-Identifier: Apache-2.0
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
        .map_or(4, std::num::NonZeroUsize::get);
    match class {
        TaskClass::Critical => (cores * 4).max(8),
        TaskClass::Realtime => (cores * 2).max(4),
        TaskClass::Sidecar => cores.max(2),
        TaskClass::Background => 2,
        TaskClass::Bulk => 1,
        TaskClass::Swarm => swarm_lane_ceiling(cores),
    }
}

/// Execution-lane ceiling for [`TaskClass::Swarm`] — a deliberately HIGH,
/// coarse runaway backstop, not the real limiter. Swarm worker concurrency is
/// bound by the memory-governed `AdmissionGate` in `origin-swarm`; this
/// semaphore exists only to keep the `spawn_in` contract intact (every task
/// acquires a permit) and to cap a pathological runaway. Overridable via
/// `ORIGIN_SWARM_LANE_MAX`; defaults to `(cores * 8).max(64)`.
fn swarm_lane_ceiling(cores: usize) -> usize {
    std::env::var("ORIGIN_SWARM_LANE_MAX")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| (cores * 8).max(64))
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
                Arc::new(Semaphore::new(permits_for(TaskClass::Swarm))),
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
