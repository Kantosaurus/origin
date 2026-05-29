// SPDX-License-Identifier: Apache-2.0
//! `spawn_in` returns a `JoinHandle` whose future executes; each class is
//! tracked by a distinct semaphore.

use origin_runtime::{init_for_test, spawn_in, TaskClass};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_in_runs_to_completion() {
    init_for_test();
    let counter = Arc::new(AtomicU32::new(0));
    let c2 = Arc::clone(&counter);
    let h = spawn_in(TaskClass::Critical, async move {
        c2.fetch_add(1, Ordering::SeqCst);
        42_u32
    });
    let v = h.await.expect("join");
    assert_eq!(v, 42);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_class_runs_independently() {
    init_for_test();
    let mut handles = Vec::new();
    for class in [
        TaskClass::Critical,
        TaskClass::Realtime,
        TaskClass::Sidecar,
        TaskClass::Background,
        TaskClass::Bulk,
    ] {
        handles.push(spawn_in(class, async move { class }));
    }
    for h in handles {
        let _ = h.await.expect("join");
    }
}
