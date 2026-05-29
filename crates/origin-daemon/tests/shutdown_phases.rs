// SPDX-License-Identifier: Apache-2.0
use origin_daemon::shutdown::{CooperativeShutdown, ShutdownPhase, ShutdownReport};
use std::time::Duration;
use tokio::sync::mpsc;

#[tokio::test(flavor = "current_thread")]
async fn all_phases_complete_in_order() {
    let (tx, mut rx) = mpsc::unbounded_channel::<ShutdownPhase>();
    let mut driver = CooperativeShutdown::for_test(tx, Duration::from_millis(50));
    let report = driver.run().await.expect("shutdown");
    assert!(matches!(report, ShutdownReport::Clean));
    let mut seen = Vec::new();
    while let Ok(p) = rx.try_recv() {
        seen.push(p);
    }
    assert_eq!(
        seen,
        vec![
            ShutdownPhase::StopAcceptingIpc,
            ShutdownPhase::CancelBulkAndBackground,
            ShutdownPhase::DrainCritical,
            ShutdownPhase::PersistSidecarQueue,
            ShutdownPhase::FlushCasWriteBuffer,
            ShutdownPhase::CheckpointSqlite,
            ShutdownPhase::CloseIpc,
            ShutdownPhase::ReleaseSharedMemoryAndArenas,
        ],
        "phases run in N8.10 order"
    );
}

#[tokio::test(flavor = "current_thread")]
#[allow(clippy::match_wildcard_for_single_variants, clippy::panic)]
async fn stuck_phase_force_advances_after_budget() {
    let (tx, _rx) = mpsc::unbounded_channel::<ShutdownPhase>();
    let mut driver =
        CooperativeShutdown::for_test_with_hang(tx, ShutdownPhase::DrainCritical, Duration::from_millis(30));
    let report = driver.run().await.expect("shutdown");
    match report {
        ShutdownReport::ForcedAdvance(phase) => {
            assert_eq!(phase, ShutdownPhase::DrainCritical);
        }
        other => panic!("expected ForcedAdvance, got {other:?}"),
    }
}
