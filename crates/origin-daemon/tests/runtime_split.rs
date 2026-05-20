//! Verify that `Critical` and `Realtime` futures execute on differently-named
//! OS threads — control-core futures land on `origin-ctrl`, worker-pool
//! futures land on tokio's default `tokio-runtime-worker-N` (or our named
//! `origin-work`) thread.

use origin_daemon::runtime_launch::{start, ShutdownSignal};
use origin_runtime::{spawn_in, TaskClass};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[test]
fn control_and_worker_run_on_distinct_threads() {
    let signal = ShutdownSignal::new();
    let signal_clone = signal.clone();
    let handle = thread::spawn(move || start(signal_clone));
    // Give the runtimes a moment to come up.
    thread::sleep(Duration::from_millis(150));

    let (ctrl_tx, ctrl_rx) = mpsc::sync_channel::<String>(1);
    let (work_tx, work_rx) = mpsc::sync_channel::<String>(1);

    // Realtime → control core
    signal.control_handle().spawn_on_control(async move {
        let name = std::thread::current().name().unwrap_or("<unnamed>").to_string();
        let _ = ctrl_tx.send(name);
    });
    // Critical → worker pool
    signal.worker_handle().spawn_on_worker(move || {
        // We're now running on a worker thread (via spawn_blocking). Capture
        // the thread name from inside the spawn_in future so we observe the
        // class-routed task's actual thread.
        let worker_handle = tokio::runtime::Handle::current();
        worker_handle.spawn(async move {
            let _permit_holder = spawn_in(TaskClass::Critical, async move {
                let name = std::thread::current().name().unwrap_or("<unnamed>").to_string();
                let _ = work_tx.send(name);
            });
        });
    });

    let ctrl = ctrl_rx.recv_timeout(Duration::from_secs(5)).expect("ctrl");
    let work = work_rx.recv_timeout(Duration::from_secs(5)).expect("work");
    assert!(ctrl.contains("origin-ctrl"), "control thread name: {ctrl}");
    assert!(
        !work.contains("origin-ctrl"),
        "worker thread name should not be origin-ctrl: {work}"
    );

    signal.trigger();
    let _ = handle.join();
}
