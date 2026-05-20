//! OAuth 2.0 helpers — PKCE generator + auth-code exchange + refresh
//! rotation, with tokens persisted through [`crate::KeyVault`].
//!
//! Scope (Phase 8.2): bare-minimum RFC 6749 §4.1 auth-code flow plus
//! §6 refresh, with PKCE (RFC 7636). Higher-level concerns — token
//! introspection, `DPoP`, the device-code flow — are out of scope.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{Error, KeyVault, Secret};

/// Account-suffix appended to the (provider, account) key so OAuth blobs do
/// not collide with raw API-key secrets stored under the same provider.
const OAUTH_SUFFIX: &str = "/oauth";

/// PKCE verifier + S256 challenge pair (RFC 7636).
///
/// The verifier is 96 bytes of CSPRNG output, base64url-no-pad encoded —
/// 128 chars, well inside the 43..=128 limit and at the upper end of the
/// allowed entropy budget.
pub struct Pkce {
    verifier: String,
    challenge: String,
}

impl Pkce {
    /// Generates a fresh PKCE pair.
    #[must_use]
    pub fn new() -> Self {
        let mut bytes = [0u8; 96];
        rand::thread_rng().fill_bytes(&mut bytes);
        let verifier = URL_SAFE_NO_PAD.encode(bytes);
        let digest = Sha256::digest(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(digest);
        Self { verifier, challenge }
    }

    /// Returns the code verifier (send this on the `/token` exchange).
    #[must_use]
    pub fn verifier(&self) -> &str {
        &self.verifier
    }

    /// Returns the S256 code challenge (send this on the authorize URL).
    #[must_use]
    pub fn challenge(&self) -> &str {
        &self.challenge
    }
}

impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}

/// Inputs for an RFC 6749 §4.1.3 authorization-code token exchange.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AuthCodeRequest {
    /// The `code` returned by the authorization server on the redirect.
    pub code: String,
    /// The PKCE verifier matching the challenge sent on the authorize URL.
    pub code_verifier: String,
    /// Must byte-equal the `redirect_uri` used to obtain `code`.
    pub redirect_uri: String,
}

impl AuthCodeRequest {
    /// Build a new auth-code exchange request. Use this rather than a struct
    /// literal so future fields can be added without a breaking change.
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        code_verifier: impl Into<String>,
        redirect_uri: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            code_verifier: code_verifier.into(),
            redirect_uri: redirect_uri.into(),
        }
    }
}

/// Result of a successful `/token` exchange, exposed to the caller. The
/// refresh token is *not* returned — it stays inside the vault.
#[derive(Debug)]
#[non_exhaustive]
pub struct ExchangedTokens {
    /// Bearer access token. Use immediately, do not log.
    pub access: Secret<String>,
    /// Unix epoch (seconds) at which `access` is expected to expire.
    pub expires_at: u64,
}

/// Outcome of [`OAuthClient::refresh`] / [`OAuthClient::refresh_if_due`].
#[derive(Debug)]
#[non_exhaustive]
pub enum RefreshOutcome {
    /// Tokens were rotated; freshly minted access token is returned.
    Rotated {
        /// Newly issued bearer access token.
        access: Secret<String>,
    },
    /// Refresh was skipped because the stored access token still has more
    /// than the requested safety window of life left.
    NotDue {
        /// Wall-clock time remaining before the token expires.
        remaining: Duration,
    },
}

/// On-disk shape persisted in the vault. Kept private — callers reach
/// tokens through [`OAuthClient::exchange`] / [`OAuthClient::refresh`].
///
/// `Zeroize` + `ZeroizeOnDrop` wipe the access and refresh token bytes
/// (both `String`) when the in-memory copy is dropped, so the secrets do
/// not linger on the heap after a refresh.
#[derive(Debug, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct StoredTokens {
    access: String,
    refresh: Option<String>,
    expires_at: u64,
}

/// Raw `/token` response. Some providers omit `refresh_token` on refresh
/// (no rotation); we fall back to the previously stored refresh token in
/// that case.
///
/// No `Debug` derive: `access_token` / `refresh_token` are bearer
/// credentials; the `xtask lint-secrets` (P11.14) gate enforces this rule
/// for any `#[derive(Debug)]` struct with a secret-looking field.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
}

/// Per-provider OAuth driver. Cheap to clone (`reqwest::Client` is `Arc`-ish).
#[derive(Debug, Clone)]
pub struct OAuthClient {
    provider: String,
    token_url: String,
    client_id: String,
    http: reqwest::Client,
}

impl OAuthClient {
    /// Constructs a new OAuth driver. `provider` is the vault namespace
    /// (e.g. `"github"`), `token_url` is the absolute `/token` endpoint,
    /// and `client_id` is the registered application identifier.
    #[must_use]
    pub fn new(
        provider: impl Into<String>,
        token_url: impl Into<String>,
        client_id: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            token_url: token_url.into(),
            client_id: client_id.into(),
            http: reqwest::Client::new(),
        }
    }

    /// Exchanges an auth-code for tokens and persists them under
    /// `(provider, "{account}/oauth")` in the supplied vault.
    ///
    /// # Errors
    /// Returns [`Error::Backend`] on transport failure or non-2xx status,
    /// and [`Error::Serde`] if the response cannot be parsed.
    pub async fn exchange(
        &self,
        vault: &KeyVault,
        account: &str,
        req: AuthCodeRequest,
    ) -> Result<ExchangedTokens, Error> {
        let form = [
            ("grant_type", "authorization_code"),
            ("code", req.code.as_str()),
            ("code_verifier", req.code_verifier.as_str()),
            ("redirect_uri", req.redirect_uri.as_str()),
            ("client_id", self.client_id.as_str()),
        ];
        let resp = self.post_form(&form).await?;
        let expires_at = now_epoch_secs().saturating_add(resp.expires_in);
        // Only persist a refresh token if the provider actually issued one;
        // storing `""` would make a later refresh POST `refresh_token=` and
        // mask the real failure mode behind a 400 from the IdP.
        self.persist(
            vault,
            account,
            &resp.access_token,
            resp.refresh_token.as_deref(),
            expires_at,
        )
        .await?;
        Ok(ExchangedTokens {
            access: Secret::new(resp.access_token),
            expires_at,
        })
    }

    /// Unconditionally refreshes the stored tokens for `account`.
    ///
    /// # Errors
    /// Returns [`Error::NotFound`] if no tokens are stored, [`Error::Serde`]
    /// on JSON parse failure, [`Error::Backend`] on HTTP failure, and
    /// [`Error::Backend`] with `"no refresh_token available …"` if the
    /// initial exchange never received a refresh token.
    pub async fn refresh(&self, vault: &KeyVault, account: &str) -> Result<RefreshOutcome, Error> {
        let stored = self.load(vault, account).await?;
        let refresh_token = stored
            .refresh
            .as_deref()
            .ok_or_else(|| Error::Backend("no refresh_token available for account".to_owned()))?;
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", self.client_id.as_str()),
        ];
        let resp = self.post_form(&form).await?;
        let expires_at = now_epoch_secs().saturating_add(resp.expires_in);
        // Some providers (e.g. Google with non-offline scopes) omit a new
        // refresh token on rotation; reuse the previous one in that case.
        let refresh = resp.refresh_token.as_deref().or(Some(refresh_token));
        self.persist(vault, account, &resp.access_token, refresh, expires_at)
            .await?;
        Ok(RefreshOutcome::Rotated {
            access: Secret::new(resp.access_token),
        })
    }

    /// Refreshes only when the stored access token is within `safety_window`
    /// of expiry. Returns [`RefreshOutcome::NotDue`] otherwise.
    ///
    /// # Errors
    /// See [`Self::refresh`].
    pub async fn refresh_if_due(
        &self,
        vault: &KeyVault,
        account: &str,
        safety_window: Duration,
    ) -> Result<RefreshOutcome, Error> {
        let stored = self.load(vault, account).await?;
        let now = now_epoch_secs();
        if stored.expires_at > now {
            let remaining = Duration::from_secs(stored.expires_at - now);
            if remaining > safety_window {
                return Ok(RefreshOutcome::NotDue { remaining });
            }
        }
        self.refresh(vault, account).await
    }

    async fn post_form(&self, form: &[(&str, &str)]) -> Result<TokenResponse, Error> {
        let resp = self
            .http
            .post(&self.token_url)
            .form(form)
            .send()
            .await
            .map_err(|e| Error::Backend(format!("oauth POST {}: {e}", self.token_url)))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| Error::Backend(format!("oauth read body: {e}")))?;
        if !status.is_success() {
            return Err(Error::Backend(format!(
                "oauth token endpoint returned {status}: {body}"
            )));
        }
        serde_json::from_str::<TokenResponse>(&body).map_err(|e| Error::Serde(format!("token response: {e}")))
    }

    async fn persist(
        &self,
        vault: &KeyVault,
        account: &str,
        access: &str,
        refresh: Option<&str>,
        expires_at: u64,
    ) -> Result<(), Error> {
        let stored = StoredTokens {
            access: access.to_owned(),
            refresh: refresh.map(str::to_owned),
            expires_at,
        };
        let json = serde_json::to_string(&stored).map_err(|e| Error::Serde(format!("encode: {e}")))?;
        vault
            .set(&self.provider, &oauth_key(account), Secret::new(json))
            .await
    }

    async fn load(&self, vault: &KeyVault, account: &str) -> Result<StoredTokens, Error> {
        let secret = vault.get(&self.provider, &oauth_key(account)).await?;
        serde_json::from_str::<StoredTokens>(secret.expose())
            .map_err(|e| Error::Serde(format!("decode: {e}")))
    }
}

fn oauth_key(account: &str) -> String {
    format!("{account}{OAUTH_SUFFIX}")
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
