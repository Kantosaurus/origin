// SPDX-License-Identifier: Apache-2.0
//! Phase callbacks installed via [`CooperativeShutdown::on`] run in N8.10
//! order, and a callback that observes its phase fires exactly once. Pairs
//! with `shutdown_phases.rs` (which exercises the test-mode no-op driver).

#![allow(clippy::panic, clippy::match_wildcard_for_single_variants)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use origin_daemon::shutdown::{CooperativeShutdown, ShutdownPhase, ShutdownReport};
use tokio::sync::mpsc;

#[tokio::test(flavor = "current_thread")]
async fn installed_callbacks_run_in_phase_order_exactly_once() {
    let (tx, mut rx) = mpsc::unbounded_channel::<ShutdownPhase>();
    let order = Arc::new(parking_lot::Mutex::new(Vec::<&'static str>::new()));
    let counter = Arc::new(AtomicU32::new(0));

    let order_a = Arc::clone(&order);
    let counter_a = Arc::clone(&counter);
    let order_b = Arc::clone(&order);
    let counter_b = Arc::clone(&counter);
    let order_c = Arc::clone(&order);

    let driver = CooperativeShutdown::for_test(tx, Duration::from_millis(100))
        .on(ShutdownPhase::PersistSidecarQueue, move || async move {
            order_a.lock().push("sidecar");
            counter_a.fetch_add(1, Ordering::Relaxed);
        })
        .on(ShutdownPhase::FlushCasWriteBuffer, move || async move {
            order_b.lock().push("cas");
            counter_b.fetch_add(1, Ordering::Relaxed);
        })
        .on(ShutdownPhase::CheckpointSqlite, move || async move {
            order_c.lock().push("sqlite");
        });

    let mut driver = driver;
    let report = driver.run().await.expect("shutdown");
    assert!(matches!(report, ShutdownReport::Clean));

    // Callbacks ran in N8.10 order:
    // PersistSidecarQueue (4th phase) → FlushCasWriteBuffer (5th) → CheckpointSqlite (6th).
    let order_seen = order.lock().clone();
    assert_eq!(order_seen, vec!["sidecar", "cas", "sqlite"]);
    // Each callback fired exactly once.
    assert_eq!(counter.load(Ordering::Relaxed), 2);

    // Phase channel still sees every phase, regardless of whether a
    // callback was installed.
    let mut seen_phases = Vec::new();
    while let Ok(p) = rx.try_recv() {
        seen_phases.push(p);
    }
    assert_eq!(seen_phases.len(), 8, "all 8 phases instrumented: {seen_phases:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn slow_callback_triggers_force_advance() {
    let (tx, _rx) = mpsc::unbounded_channel::<ShutdownPhase>();
    let driver = CooperativeShutdown::for_test(tx, Duration::from_millis(20)).on(
        ShutdownPhase::DrainCritical,
        || async {
            tokio::time::sleep(Duration::from_secs(60)).await;
        },
    );

    let mut driver = driver;
    let report = driver.run().await.expect("shutdown");
    match report {
        ShutdownReport::ForcedAdvance(phase) => assert_eq!(phase, ShutdownPhase::DrainCritical),
        other => panic!("expected ForcedAdvance, got {other:?}"),
    }
}
