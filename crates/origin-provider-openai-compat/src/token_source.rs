//! Pluggable bearer-token source for the OpenAI-compat client.
//!
//! Synchronous API-key lookups use `StaticBearer` / `StaticHeader`; OAuth
//! refresh paths can ship a custom impl that hits `OAuthClient::refresh_if_due`
//! before returning the access token.

use async_trait::async_trait;
use std::sync::Arc;

#[async_trait]
pub trait TokenSource: Send + Sync + std::fmt::Debug {
    /// Returns `(header_name, header_value)`. Empty `header_name` means
    /// "no auth header" (used by Ollama/vLLM/SGLang catalog rows).
    ///
    /// # Errors
    /// Returns [`TokenError`] if the token cannot be retrieved (e.g. OAuth
    /// refresh failure or missing credential).
    async fn header(&self) -> Result<(String, String), TokenError>;
}

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("auth: {0}")]
    Auth(String),
}

/// Static `Authorization: Bearer <key>` source. Used for every API-key
/// provider where the catalog row says `header: "Authorization", prefix: "Bearer "`.
#[derive(Debug, Clone)]
pub struct StaticBearer {
    key: String,
}

impl StaticBearer {
    #[must_use]
    pub fn new(key: impl Into<String>) -> Arc<Self> {
        Arc::new(Self { key: key.into() })
    }
}

#[async_trait]
impl TokenSource for StaticBearer {
    async fn header(&self) -> Result<(String, String), TokenError> {
        Ok(("Authorization".to_string(), format!("Bearer {}", self.key)))
    }
}

/// Static `<header>: <prefix><key>` source. Used for catalog rows with
/// non-standard auth headers (`x-api-key`, `api-key`, `x-goog-api-key`, …).
#[derive(Debug, Clone)]
pub struct StaticHeader {
    header: String,
    prefix: String,
    key: String,
}

impl StaticHeader {
    #[must_use]
    pub fn new(header: impl Into<String>, prefix: impl Into<String>, key: impl Into<String>) -> Arc<Self> {
        Arc::new(Self { header: header.into(), prefix: prefix.into(), key: key.into() })
    }
}

#[async_trait]
impl TokenSource for StaticHeader {
    async fn header(&self) -> Result<(String, String), TokenError> {
        if self.header.is_empty() {
            return Ok((String::new(), String::new()));
        }
        Ok((self.header.clone(), format!("{}{}", self.prefix, self.key)))
    }
}

/// "No auth" source for local providers (Ollama, vLLM, SGLang).
#[derive(Debug, Clone)]
pub struct NoAuth;

impl NoAuth {
    #[must_use]
    pub fn new() -> Arc<Self> { Arc::new(Self) }
}

impl Default for NoAuth {
    fn default() -> Self { Self }
}

#[async_trait]
impl TokenSource for NoAuth {
    async fn header(&self) -> Result<(String, String), TokenError> {
        Ok((String::new(), String::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn static_bearer_emits_authorization() {
        let s = StaticBearer::new("sk-abc");
        let (h, v) = s.header().await.unwrap();
        assert_eq!(h, "Authorization");
        assert_eq!(v, "Bearer sk-abc");
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn static_header_with_prefix() {
        let s = StaticHeader::new("x-api-key", "", "abc123");
        let (h, v) = s.header().await.unwrap();
        assert_eq!(h, "x-api-key");
        assert_eq!(v, "abc123");
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn no_auth_returns_empty() {
        let s = NoAuth::new();
        let (h, v) = s.header().await.unwrap();
        assert!(h.is_empty());
        assert!(v.is_empty());
    }
}
