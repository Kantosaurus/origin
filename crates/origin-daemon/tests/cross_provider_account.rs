// SPDX-License-Identifier: Apache-2.0
//! Cross-provider `/account`-switch credential sync.
//!
//! When the daemon rebuilds a provider mid-loop for a cross-provider routing
//! pick, it must resolve credentials for the *currently switched* account, not
//! the startup default. `provider_factory::set_global` registers the account
//! once; `update_global_account` (called from the `/account` switch handler)
//! updates which account subsequent rebuilds resolve against.
//!
//! This lives in its own integration-test binary because `set_global` writes a
//! process-wide `OnceLock` — the unit-test binary asserts the *unregistered*
//! path and must never have it set.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use origin_daemon::provider_factory::{
    build_provider_for, build_provider_for_account, get_current_account, set_global,
    update_global_account, ProviderFactory,
};
use origin_keyvault::KeyVault;
use origin_provider::catalog::Catalog;

/// One combined test because `set_global` writes a process-wide `OnceLock`
/// (idempotent: only the first registration wins), so a single test owns the
/// registration for the whole integration-test binary.
#[tokio::test]
async fn per_connection_accounts_resolve_distinct_credentials() {
    // Seed credentials so the Anthropic ApiKey wire can build offline for two
    // DISTINCT accounts, but deliberately leave a third account uncredentialed.
    let vault = KeyVault::in_memory();
    vault
        .set(
            "anthropic",
            "work",
            origin_keyvault::Secret::new("sk-work".to_string()),
        )
        .await
        .expect("seed work credential");
    vault
        .set(
            "anthropic",
            "personal",
            origin_keyvault::Secret::new("sk-personal".to_string()),
        )
        .await
        .expect("seed personal credential");
    // NB: NO credential seeded for account "default" or "ghost".
    let factory = ProviderFactory::new(vault, Catalog::builtin());

    // Register the factory with the startup default account.
    set_global(Arc::new(factory), "default");
    assert_eq!(
        get_current_account().as_deref(),
        Some("default"),
        "registered account should be readable"
    );

    // --- Per-connection isolation: the load-bearing assertion. ---
    // The GLOBAL account stays "default" (which has NO credential) for the whole
    // of this block. Two different connections rebuild with their OWN account via
    // `build_provider_for_account`, resolving DIFFERENT credentials, while the
    // global free `build_provider_for` (which reads the "default" slot) fails.
    //
    // Before per-connection plumbing existed there was no
    // `build_provider_for_account`, so a rebuild could only read the single
    // global slot and both connections would share one account.
    assert!(
        build_provider_for_account("anthropic", "m", "work")
            .await
            .is_some(),
        "connection on account `work` must resolve its own credential"
    );
    assert!(
        build_provider_for_account("anthropic", "m", "personal")
            .await
            .is_some(),
        "connection on account `personal` must resolve its own credential"
    );
    assert!(
        build_provider_for_account("anthropic", "m", "ghost")
            .await
            .is_none(),
        "an account with no credential must NOT resolve (proves per-account lookup)"
    );
    // The global/default path (account slot still "default", uncredentialed)
    // must fall back to None — unaffected by the per-connection accounts above.
    assert!(
        build_provider_for("anthropic", "m").await.is_none(),
        "the global default account (uncredentialed) must stay None"
    );

    // --- Global account slot still behaves exactly as before. ---
    // A `/account` switch updates the account a subsequent GLOBAL rebuild resolves.
    update_global_account("work");
    assert_eq!(
        get_current_account().as_deref(),
        Some("work"),
        "account switch must propagate to the global factory"
    );
    // Now the global path resolves `work`'s credential (which exists).
    assert!(
        build_provider_for("anthropic", "m").await.is_some(),
        "after switching the global slot to `work`, the global rebuild resolves it"
    );

    // Switching again keeps following the latest account (self-healing).
    update_global_account("personal");
    assert_eq!(get_current_account().as_deref(), Some("personal"));
}
