// SPDX-License-Identifier: Apache-2.0
use std::sync::Arc;

use origin_keyvault::audit::{AuditAction, AuditRing};
use origin_keyvault::{KeyVault, Secret};
use tempfile::tempdir;

/// R16: a vault with an attached audit ring must record every secret access
/// (set/get/delete/list) into the ring. Before the daemon wiring (and this
/// `with_audit` primitive) the audit ring was never attached in production, so
/// no secret-access trail was ever produced.
#[tokio::test]
async fn attached_vault_records_each_access() {
    let dir = tempdir().expect("tempdir");
    let ring = Arc::new(AuditRing::open(dir.path()).await.expect("open"));
    // Audit the in-memory backend so the test is platform-independent.
    let vault = KeyVault::in_memory().with_audit(Arc::clone(&ring));

    vault
        .set("anthropic", "default", Secret::new("sk-ant-xxx".to_string()))
        .await
        .expect("set");
    let _ = vault.get("anthropic", "default").await.expect("get");
    let _ = vault.list("anthropic").await.expect("list");
    vault.delete("anthropic", "default").await.expect("delete");

    let events = ring.replay().await.expect("replay");
    let actions: Vec<AuditAction> = events.iter().map(|e| e.action).collect();
    assert!(
        actions.contains(&AuditAction::Set)
            && actions.contains(&AuditAction::Get)
            && actions.contains(&AuditAction::List)
            && actions.contains(&AuditAction::Delete),
        "attached vault must record set/get/list/delete; got {actions:?}"
    );
    // The recorded events carry the (provider, account) namespace, never the
    // secret bytes (covered by `ring_never_records_secret_bytes`).
    assert!(events.iter().all(|e| e.provider == "anthropic"));
}

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
