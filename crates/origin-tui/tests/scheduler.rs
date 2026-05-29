// SPDX-License-Identifier: Apache-2.0
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use origin_tui::scheduler::Scheduler;

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ten_dirty_flips_in_one_budget_yield_one_render() {
    let frames = Arc::new(AtomicU32::new(0));
    let frames_in = frames.clone();
    let sched = Scheduler::new(Duration::from_millis(6));
    let handle = sched.handle();
    let task = tokio::spawn(async move {
        sched
            .run(move || {
                frames_in.fetch_add(1, Ordering::Relaxed);
            })
            .await;
    });

    for _ in 0..10 {
        handle.mark_dirty();
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
    task.abort();
    let count = frames.load(Ordering::Relaxed);
    assert_eq!(count, 1, "expected exactly one render frame, got {count}");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn no_dirty_flip_means_zero_renders() {
    let frames = Arc::new(AtomicU32::new(0));
    let frames_in = frames.clone();
    let sched = Scheduler::new(Duration::from_millis(6));
    let task = tokio::spawn(async move {
        sched
            .run(move || {
                frames_in.fetch_add(1, Ordering::Relaxed);
            })
            .await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    task.abort();
    assert_eq!(frames.load(Ordering::Relaxed), 0);
}
