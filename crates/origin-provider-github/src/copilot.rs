// SPDX-License-Identifier: Apache-2.0
//! GitHub **Copilot** chat provider.
//!
//! Copilot's chat API is OpenAI-shaped (`POST /chat/completions`) but is NOT
//! called with the raw GitHub OAuth token. Instead a short-lived *Copilot
//! session token* must be minted by exchanging the stored GitHub OAuth token
//! (the `ghu_…` token from the device flow under client id
//! `Iv1.b507a08c87ecfe98`) at `GET api.github.com/copilot_internal/v2/token`,
//! and the chat request must carry the editor-identity headers Copilot expects
//! (`Copilot-Integration-Id`, `Editor-Version`, …).
//!
//! This module reuses [`origin_provider_openai_compat::OpenAiCompat`] for the
//! actual chat/streaming/tool wire (it is plain `OpenAI` shape) and supplies the
//! token exchange + headers via a custom [`TokenSource`].

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use origin_keyvault::KeyVault;
use origin_provider_openai_compat::{OpenAiCompat, OpenAiCompatConfig, TokenError, TokenSource};
use serde::Deserialize;

/// GitHub Copilot session-token exchange endpoint.
const TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
/// Default Copilot chat host (individual plan). Business/enterprise plans use
/// `api.business.githubcopilot.com` / `api.enterprise.githubcopilot.com` — the
/// token response's `endpoints.api` is authoritative, but individual is the
/// common case and the catalog default.
const DEFAULT_CHAT_BASE: &str = "https://api.individual.githubcopilot.com";
const CHAT_PATH: &str = "/chat/completions";

// Editor-identity headers Copilot requires. Values mirror a recent VS Code +
// Copilot Chat client; Copilot validates their presence, not exact versions.
const EDITOR_VERSION: &str = "vscode/1.85.1";
const EDITOR_PLUGIN_VERSION: &str = "copilot/1.155.0";
const USER_AGENT: &str = "GithubCopilot/1.155.0";
const INTEGRATION_ID: &str = "vscode-chat";

/// Refresh the session token this many seconds before its stated expiry, so an
/// in-flight request never races the boundary.
const EXPIRY_MARGIN_SECS: u64 = 60;

/// Build a GitHub Copilot chat provider for `account`'s stored credentials.
///
/// The returned [`OpenAiCompat`] talks `OpenAI` chat/completions (streaming,
/// tools, usage) against the Copilot host, authenticated by the session-token
/// exchange and carrying the required editor headers.
#[must_use]
pub fn provider(vault: KeyVault, account: impl Into<String>) -> OpenAiCompat {
    provider_with_base(vault, account, DEFAULT_CHAT_BASE)
}

/// As [`provider`], but against an explicit chat base URL (testing / enterprise).
#[must_use]
pub fn provider_with_base(vault: KeyVault, account: impl Into<String>, base: &str) -> OpenAiCompat {
    let auth = Arc::new(CopilotTokenSource::new(vault, account));
    OpenAiCompat::new(OpenAiCompatConfig {
        name: "github-copilot",
        base_url: base.trim_end_matches('/').to_string(),
        chat_path: CHAT_PATH.to_string(),
        auth,
        extra_headers: vec![
            ("Copilot-Integration-Id".to_string(), INTEGRATION_ID.to_string()),
            ("Editor-Version".to_string(), EDITOR_VERSION.to_string()),
            ("Editor-Plugin-Version".to_string(), EDITOR_PLUGIN_VERSION.to_string()),
            ("User-Agent".to_string(), USER_AGENT.to_string()),
        ],
    })
}

/// A [`TokenSource`] that returns `Authorization: Bearer <copilot session
/// token>`, minting and caching the session token via the Copilot exchange.
#[derive(Debug)]
struct CopilotTokenSource {
    vault: KeyVault,
    account: String,
    client: reqwest::Client,
    cache: Mutex<Option<Session>>,
}

#[derive(Clone, Debug)]
struct Session {
    token: String,
    expires_at: u64,
}

impl CopilotTokenSource {
    fn new(vault: KeyVault, account: impl Into<String>) -> Self {
        Self {
            vault,
            account: account.into(),
            client: reqwest::Client::new(),
            cache: Mutex::new(None),
        }
    }

    /// The exchange endpoint. Honors `ORIGIN_COPILOT_TOKEN_URL` so tests can
    /// redirect it to a mock server; defaults to the real GitHub endpoint.
    fn token_url() -> String {
        std::env::var("ORIGIN_COPILOT_TOKEN_URL").unwrap_or_else(|_| TOKEN_URL.to_string())
    }

    /// Return a still-valid cached session token, if any.
    fn cached_token(&self) -> Option<String> {
        let now = now_secs();
        let guard = self.cache.lock().ok()?;
        guard
            .as_ref()
            .filter(|s| s.expires_at > now.saturating_add(EXPIRY_MARGIN_SECS))
            .map(|s| s.token.clone())
    }

    /// Exchange the stored GitHub OAuth token for a fresh Copilot session token
    /// and cache it. The lock is never held across the `await`s.
    async fn refresh(&self) -> Result<String, TokenError> {
        let secret = self
            .vault
            .get("github-copilot", &format!("{}/oauth", self.account))
            .await
            .map_err(|e| TokenError::Auth(format!("vault read: {e}")))?;
        let stored: Stored = serde_json::from_str(secret.expose())
            .map_err(|e| TokenError::Auth(format!("stored token parse: {e}")))?;

        let resp = self
            .client
            .get(Self::token_url())
            // The exchange uses the `token` scheme, NOT `Bearer`.
            .header("authorization", format!("token {}", stored.access))
            .header("accept", "application/json")
            .header("editor-version", EDITOR_VERSION)
            .header("editor-plugin-version", EDITOR_PLUGIN_VERSION)
            .header("user-agent", USER_AGENT)
            .send()
            .await
            .map_err(|e| TokenError::Auth(format!("copilot token exchange: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(TokenError::Auth(format!(
                "copilot token exchange {status}: {body}"
            )));
        }
        let tok: TokenResponse = resp
            .json()
            .await
            .map_err(|e| TokenError::Auth(format!("copilot token decode: {e}")))?;

        if let Ok(mut guard) = self.cache.lock() {
            *guard = Some(Session {
                token: tok.token.clone(),
                expires_at: tok.expires_at,
            });
        }
        Ok(tok.token)
    }
}

#[async_trait]
impl TokenSource for CopilotTokenSource {
    async fn header(&self) -> Result<(String, String), TokenError> {
        let token = match self.cached_token() {
            Some(t) => t,
            None => self.refresh().await?,
        };
        Ok(("Authorization".to_string(), format!("Bearer {token}")))
    }
}

/// On-disk OAuth blob shape (we only need the access token).
#[derive(Deserialize)]
struct Stored {
    access: String,
}

/// Copilot token-exchange response (only the fields we use).
#[derive(Deserialize)]
struct TokenResponse {
    token: String,
    #[serde(default)]
    expires_at: u64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
