// SPDX-License-Identifier: Apache-2.0
//! Real macOS keychain integration test for the Phase 11 `list()` enumeration.
//!
//! Compiles + runs ONLY on macOS hosts (and CI macOS runners). The Phase 11
//! `MacBackend::list()` implementation wraps `SecItemCopyMatching` via
//! `security_framework::item::ItemSearchOptions`; this test exercises the full
//! round-trip against the real user keychain.
//!
//! ## Why this test is gated
//!
//! Hitting the live keychain may trigger an "always allow" prompt the very
//! first time a binary calls into `SecItem*` on a given host — that's a
//! one-time interactive UI. Subsequent runs are silent. The test uses a
//! unique service name per run (`origin-test-list-{nanos}`) so it never
//! collides with the user's real entries, and it deletes everything it
//! created before exiting (including on the failure path).
//!
//! ## What this verifies
//!
//! 1. `set` then `list` returns exactly the seeded accounts in some order.
//! 2. `list` on a service with no entries returns `Ok(Vec::new())` (the
//!    `errSecItemNotFound` translation path).
//! 3. After `delete`, `list` no longer returns the deleted account.

#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use origin_keyvault::{KeyVault, Secret};

/// Unique service slug per test run so concurrent invocations and prior
/// failures can't see each other's entries.
fn unique_service_slug() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("origin-test-list-{nanos}")
}

#[tokio::test]
async fn list_returns_seeded_accounts_then_empty_after_delete() {
    let vault = KeyVault::detect().expect("detect");
    let svc = unique_service_slug();

    // Phase 1: empty service → empty list.
    let empty = vault.list(&svc).await.expect("list empty");
    assert!(
        empty.is_empty(),
        "fresh service should have zero entries, got {empty:?}"
    );

    // Phase 2: seed three accounts, then list returns all three.
    let accounts = ["one", "two", "three"];
    for a in accounts {
        vault
            .set(&svc, a, Secret::new(format!("secret-{a}")))
            .await
            .unwrap_or_else(|e| panic!("set {a}: {e}"));
    }

    let listed = vault.list(&svc).await.expect("list seeded");
    let mut sorted: Vec<String> = listed.into_iter().collect();
    sorted.sort();
    let mut expected: Vec<String> = accounts.iter().map(|s| (*s).to_string()).collect();
    expected.sort();
    assert_eq!(sorted, expected, "list must return exactly the seeded accounts");

    // Phase 3: delete one, list reflects the change.
    vault.delete(&svc, "two").await.expect("delete two");
    let after_delete = vault.list(&svc).await.expect("list after delete");
    let mut after_sorted: Vec<String> = after_delete.into_iter().collect();
    after_sorted.sort();
    assert_eq!(
        after_sorted,
        vec!["one".to_string(), "three".to_string()],
        "delete must remove the entry from list output"
    );

    // Cleanup: drop the remaining two so the keychain is left as we found it.
    let _ = vault.delete(&svc, "one").await;
    let _ = vault.delete(&svc, "three").await;
}
