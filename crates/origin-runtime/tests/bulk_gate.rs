// SPDX-License-Identifier: Apache-2.0
//! Bulk tasks are parked while at least one Critical task is in flight.

use origin_runtime::{critical_tasks_in_flight, init_for_test, spawn_in, TaskClass};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout, Duration};

/// Poll the Critical busy-counter until at least one Critical task holds its
/// permit, instead of guessing a sleep interval. The counter is bumped *after*
/// `spawn_in` acquires the Critical semaphore permit, so observing `>= 1` is a
/// deterministic proof that the Critical task is genuinely in flight.
async fn await_critical_in_flight() {
    timeout(Duration::from_secs(5), async {
        while critical_tasks_in_flight() < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a Critical task should acquire its permit promptly");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bulk_parks_while_critical_runs() {
    init_for_test();
    let bulk_ran = Arc::new(AtomicBool::new(false));
    let bulk_ran_clone = Arc::clone(&bulk_ran);
    let critical_done = Arc::new(AtomicBool::new(false));
    let critical_done_clone = Arc::clone(&critical_done);

    // The Critical task holds its permit until we explicitly release it, so the
    // window during which Bulk must stay parked is bounded by synchronization,
    // not a fixed sleep.
    let (release_critical, hold) = oneshot::channel::<()>();
    let critical = spawn_in(TaskClass::Critical, async move {
        // Park here (permit held) until the test signals release.
        let _ = hold.await;
        critical_done_clone.store(true, Ordering::SeqCst);
    });

    // Deterministically wait until the Critical task has acquired its permit
    // (busy-counter >= 1) before spawning Bulk — no 20ms guess.
    await_critical_in_flight().await;

    // Spawn a Bulk task — it must stay parked while Critical is in flight.
    let bulk = spawn_in(TaskClass::Bulk, async move {
        bulk_ran_clone.store(true, Ordering::SeqCst);
    });

    // While the Critical task provably still holds its permit, Bulk must not run.
    // Yield generously to give a (buggy) Bulk task every chance to slip through.
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert!(
        critical_tasks_in_flight() >= 1,
        "test invariant: Critical still holds its permit at the check point"
    );
    assert!(
        !bulk_ran.load(Ordering::SeqCst),
        "Bulk must be parked while Critical is in flight"
    );

    // Release the Critical task; Bulk must then run to completion.
    let _ = release_critical.send(());
    let _ = critical.await;
    let _ = bulk.await;
    assert!(critical_done.load(Ordering::SeqCst));
    assert!(bulk_ran.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_bulk_under_repeated_critical() {
    init_for_test();
    let counter = Arc::new(AtomicU32::new(0));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let c = Arc::clone(&counter);
        handles.push(spawn_in(TaskClass::Bulk, async move {
            c.fetch_add(1, Ordering::SeqCst);
        }));
    }
    for _ in 0..4 {
        handles.push(spawn_in(TaskClass::Critical, async move {
            sleep(Duration::from_millis(30)).await;
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    assert_eq!(counter.load(Ordering::SeqCst), 8, "every Bulk eventually runs");
}
