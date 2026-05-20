use origin_keyvault::audit::{AuditAction, AuditRing};
use tempfile::tempdir;

#[tokio::test]
async fn ring_appends_and_replays() {
    let dir = tempdir().expect("tempdir");
    let ring = AuditRing::open(dir.path()).await.expect("open");
    ring.record(AuditAction::Set, "anthropic", "default")
        .await
        .expect("rec");
    ring.record(AuditAction::Get, "anthropic", "default")
        .await
        .expect("rec");
    ring.record(AuditAction::Delete, "anthropic", "default")
        .await
        .expect("rec");

    let events = ring.replay().await.expect("replay");
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].action, AuditAction::Set);
    assert_eq!(events[0].provider, "anthropic");
    assert_eq!(events[0].account, "default");
}

#[tokio::test]
async fn ring_never_records_secret_bytes() {
    let dir = tempdir().expect("tempdir");
    let ring = AuditRing::open(dir.path()).await.expect("open");
    ring.record(AuditAction::Set, "anthropic", "default")
        .await
        .expect("rec");
    let events = ring.replay().await.expect("replay");
    // Field schema: action + provider + account + timestamp; no `secret` field.
    let json = serde_json::to_string(&events[0]).expect("ser");
    assert!(
        !json.contains("sk-"),
        "secret token must never appear in audit: {json}"
    );
    assert!(
        !json.contains("Bearer"),
        "auth header must never appear in audit: {json}"
    );
}

#[tokio::test]
async fn ring_rotates_after_30_days_worth_of_entries() {
    // Use an aggressively-small page size so the test runs in <1s; real config
    // is 8 MiB per page * 30 days.
    let dir = tempdir().expect("tempdir");
    let ring = AuditRing::open_with_page_size(dir.path(), 1024)
        .await
        .expect("open");
    for i in 0..500 {
        ring.record(AuditAction::Get, "anthropic", &format!("acct-{i}"))
            .await
            .expect("rec");
    }
    let pages: Vec<_> = std::fs::read_dir(dir.path()).expect("readdir").collect();
    assert!(
        pages.len() >= 2,
        "expected >=2 pages after rotation, got {}",
        pages.len()
    );
}
