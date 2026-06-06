// SPDX-License-Identifier: Apache-2.0
//! Google OAuth credentials, loaded from [`origin_keyvault::KeyVault`].
//!
//! The credential blob is a small JSON object persisted under a
//! `(provider, account)` pair (default `("google", "gmail")`):
//!
//! ```json
//! { "client_id": "...", "client_secret": "...", "refresh_token": "..." }
//! ```
//!
//! All three fields are bearer secrets. They are held in memory as
//! [`origin_keyvault::Secret<String>`] so they zeroize on drop and never
//! appear in `Debug`/`Display`. This module hand-writes a redacting `Debug`
//! impl rather than deriving it, so the whole struct is safe to log.

use origin_keyvault::Secret;
use serde::Deserialize;

use crate::error::{Error, Result};

/// On-disk credential blob, parsed straight from the keyvault JSON. Kept
/// private and immediately consumed into [`Credentials`]; the plain `String`
/// fields here live only for the duration of `serde_json::from_str`.
///
/// No `Debug` derive: the fields are bearer secrets and the `xtask
/// lint-secrets` gate forbids `#[derive(Debug)]` on secret-named string
/// fields that are not wrapped in `Secret<…>`.
#[derive(Deserialize)]
struct RawCreds {
    client_id: String,
    client_secret: String,
    refresh_token: String,
}

/// Parsed, redaction-safe Google OAuth credentials.
///
/// Does **not** implement `Clone` (mirrors `Secret<T>`), `Serialize`, or a
/// derived `Debug`. Construct via [`Credentials::from_json`].
pub struct Credentials {
    client_id: Secret<String>,
    client_secret: Secret<String>,
    refresh_token: Secret<String>,
}

impl Credentials {
    /// Parse a credential blob from its JSON representation.
    ///
    /// # Errors
    /// Returns [`Error::CredentialFormat`] if the JSON is missing any of
    /// `client_id`, `client_secret`, or `refresh_token`.
    pub fn from_json(json: &str) -> Result<Self> {
        let raw: RawCreds = serde_json::from_str(json).map_err(|e| Error::CredentialFormat(e.to_string()))?;
        Ok(Self {
            client_id: Secret::new(raw.client_id),
            client_secret: Secret::new(raw.client_secret),
            refresh_token: Secret::new(raw.refresh_token),
        })
    }

    /// The registered OAuth client identifier. Treat as a secret.
    #[must_use]
    pub fn client_id(&self) -> &str {
        self.client_id.expose()
    }

    /// The OAuth client secret. Treat as a secret; never log.
    #[must_use]
    pub fn client_secret(&self) -> &str {
        self.client_secret.expose()
    }

    /// The long-lived refresh token. Treat as a secret; never log.
    #[must_use]
    pub fn refresh_token(&self) -> &str {
        self.refresh_token.expose()
    }
}

impl core::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never include any field value — all three are bearer secrets.
        f.debug_struct("Credentials")
            .field("client_id", &"<redacted>")
            .field("client_secret", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "client_id": "1234.apps.googleusercontent.com",
        "client_secret": "GOCSPX-supersecret",
        "refresh_token": "1//refresh-token-value"
    }"#;

    #[test]
    fn parses_all_three_fields() {
        let c = Credentials::from_json(SAMPLE).unwrap();
        assert_eq!(c.client_id(), "1234.apps.googleusercontent.com");
        assert_eq!(c.client_secret(), "GOCSPX-supersecret");
        assert_eq!(c.refresh_token(), "1//refresh-token-value");
    }

    #[test]
    fn missing_field_is_format_error() {
        let bad = r#"{"client_id":"x","client_secret":"y"}"#;
        let err = Credentials::from_json(bad).unwrap_err();
        assert!(matches!(err, Error::CredentialFormat(_)));
    }

    #[test]
    fn debug_redacts_every_secret() {
        let c = Credentials::from_json(SAMPLE).unwrap();
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("GOCSPX-supersecret"), "client_secret leaked: {dbg}");
        assert!(
            !dbg.contains("1//refresh-token-value"),
            "refresh_token leaked: {dbg}"
        );
        assert!(!dbg.contains("googleusercontent"), "client_id leaked: {dbg}");
        assert!(dbg.contains("<redacted>"));
    }
}
