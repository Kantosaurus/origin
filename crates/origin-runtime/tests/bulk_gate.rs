// SPDX-License-Identifier: Apache-2.0
//! Bulk tasks are parked while at least one Critical task is in flight.

use origin_runtime::{init_for_test, spawn_in, TaskClass};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use tokio::time::{sleep, Duration};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bulk_parks_while_critical_runs() {
    init_for_test();
    let bulk_ran = Arc::new(AtomicBool::new(false));
    let bulk_ran_clone = Arc::clone(&bulk_ran);
    let critical_done = Arc::new(AtomicBool::new(false));
    let critical_done_clone = Arc::clone(&critical_done);

    // Kick off a long-running Critical task first.
    let critical = spawn_in(TaskClass::Critical, async move {
        sleep(Duration::from_millis(150)).await;
        critical_done_clone.store(true, Ordering::SeqCst);
    });
    // Give the Critical task a chance to acquire its permit.
    sleep(Duration::from_millis(20)).await;
    // Spawn a Bulk task — it must wait until critical_done == true.
    let bulk = spawn_in(TaskClass::Bulk, async move {
        bulk_ran_clone.store(true, Ordering::SeqCst);
    });
    // Halfway through the Critical task, Bulk must NOT have run yet.
    sleep(Duration::from_millis(60)).await;
    assert!(
        !bulk_ran.load(Ordering::SeqCst),
        "Bulk must be parked while Critical is in flight"
    );
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
