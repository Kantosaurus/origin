use origin_tui::scheduler::Scheduler;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

#[tokio::test(start_paused = true)]
async fn ten_dirty_flips_within_6ms_coalesce_to_one_frame() {
    let frames = Arc::new(AtomicU32::new(0));
    let s = Scheduler::new(std::time::Duration::from_millis(6));
    let h = s.handle();

    let f = frames.clone();
    let task = tokio::spawn(async move {
        s.run(move || {
            f.fetch_add(1, Ordering::SeqCst);
        })
        .await;
    });

    for _ in 0..10 {
        h.mark_dirty();
    }

    tokio::time::advance(std::time::Duration::from_millis(7)).await;
    tokio::task::yield_now().await;

    task.abort();
    let _ = task.await;

    let n = frames.load(Ordering::SeqCst);
    assert!(n <= 1, "expected at most 1 frame, got {n}");
}

#[tokio::test(start_paused = true)]
async fn no_dirty_means_no_render() {
    let frames = Arc::new(AtomicU32::new(0));
    let s = Scheduler::new(std::time::Duration::from_millis(6));

    let f = frames.clone();
    let task = tokio::spawn(async move {
        s.run(move || {
            f.fetch_add(1, Ordering::SeqCst);
        })
        .await;
    });

    tokio::time::advance(std::time::Duration::from_millis(100)).await;
    tokio::task::yield_now().await;

    task.abort();
    let _ = task.await;

    assert_eq!(frames.load(Ordering::SeqCst), 0);
}
