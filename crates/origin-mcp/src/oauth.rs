//! Bridge from `origin_keyvault::KeyVault` to `HttpTransport`'s bearer slot.
//!
//! Tokens are stored under `(provider="mcp-<server>", account="<id>/oauth")`,
//! matching the suffix convention `origin-keyvault::oauth` uses for its own
//! token blobs. P10.11 just reads the bearer back and pushes it onto the
//! transport; the refresh dance lives entirely in the vault crate.

use crate::transport_http::HttpTransport;
use origin_keyvault::{Error as KvError, KeyVault};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OAuthBridgeError {
    #[error("vault: {0}")]
    Vault(#[from] KvError),
}

/// Look up the OAuth bearer for `(provider, account)` and set it on `transport`.
///
/// # Errors
/// Forwards [`KvError`] if the secret is missing or the backend fails.
pub async fn attach_bearer(
    vault: &KeyVault,
    provider: &str,
    account: &str,
    transport: &Arc<HttpTransport>,
) -> Result<(), OAuthBridgeError> {
    let key = format!("{account}/oauth");
    let secret = vault.get(provider, &key).await?;
    let token = secret.expose().to_string();
    transport.set_bearer(Some(token));
    Ok(())
}
