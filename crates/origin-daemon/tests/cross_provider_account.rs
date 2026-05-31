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
    get_current_account, set_global, update_global_account, ProviderFactory,
};
use origin_keyvault::KeyVault;
use origin_provider::catalog::Catalog;

#[tokio::test]
async fn account_switch_updates_the_account_used_for_cross_provider_rebuild() {
    let factory = ProviderFactory::new(KeyVault::in_memory(), Catalog::builtin());

    // Register the factory with the startup default account.
    set_global(Arc::new(factory), "default");
    assert_eq!(
        get_current_account().as_deref(),
        Some("default"),
        "registered account should be readable"
    );

    // A `/account` switch updates the account a subsequent rebuild resolves.
    update_global_account("work");
    assert_eq!(
        get_current_account().as_deref(),
        Some("work"),
        "account switch must propagate to the global factory"
    );

    // Switching again keeps following the latest account (self-healing).
    update_global_account("personal");
    assert_eq!(get_current_account().as_deref(), Some("personal"));
}
