// SPDX-License-Identifier: Apache-2.0
//! `WebSearch` via Tavily.
//!
//! Tavily was chosen over Brave/SerpAPI for LLM-friendly snippets and a
//! single-endpoint API. The API key is resolved in this order:
//!
//! 1. The OS keyvault, under `tavily:default` — what `origin init` writes.
//! 2. The `TAVILY_API_KEY` environment variable — legacy fallback.
//!
//! The `_with_endpoint` variant takes the key directly so tests can point at
//! a local server without touching the vault or env.

use origin_keyvault::KeyVault;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const TAVILY_ENDPOINT: &str = "https://api.tavily.com/search";

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub api_key: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("no Tavily API key in vault (tavily:default) or TAVILY_API_KEY env — run `origin init` or `origin keyring add tavily default <key>`")]
    NoApiKey,
    #[error("keyvault: {0}")]
    Vault(String),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("parse: {0}")]
    Parse(String),
}

#[derive(Serialize)]
struct ReqBody<'a> {
    api_key: &'a str,
    query: &'a str,
    max_results: usize,
    /// Ask Tavily to synthesize a single source-grounded answer so the model can
    /// use it directly instead of re-deriving one from the raw snippets — fewer
    /// tokens spent reading hits, and a more accurate, cited result.
    include_answer: bool,
    /// `"basic"` (default, cheaper) is sufficient for the LLM-snippet use case;
    /// avoids the higher per-call cost of `"advanced"`.
    search_depth: &'static str,
}
#[derive(Deserialize)]
struct RawResp {
    results: Vec<RawHit>,
    /// The grounded answer, present only when `include_answer` was requested and
    /// Tavily produced one. `default` keeps older/mocked responses parseable.
    #[serde(default)]
    answer: Option<String>,
}
#[derive(Deserialize)]
struct RawHit {
    title: String,
    url: String,
    content: String,
}

/// Resolve the Tavily API key from the vault, falling back to env.
///
/// Tries `vault.get("tavily", "default")` first; if absent, reads
/// `TAVILY_API_KEY` from the process environment. Returns
/// [`SearchError::NoApiKey`] when neither source has a value.
///
/// # Errors
/// [`SearchError::Vault`] for backend failures other than `NotFound`;
/// [`SearchError::NoApiKey`] when no key is configured anywhere.
pub async fn resolve_api_key(vault: &KeyVault) -> Result<String, SearchError> {
    match vault.get("tavily", "default").await {
        Ok(secret) => Ok(secret.expose().clone()),
        Err(origin_keyvault::Error::NotFound { .. }) => {
            std::env::var("TAVILY_API_KEY").map_err(|_| SearchError::NoApiKey)
        }
        Err(e) => Err(SearchError::Vault(e.to_string())),
    }
}

/// Search Tavily; resolves the API key via [`resolve_api_key`] against the
/// platform-detected keyvault, then falls back to `TAVILY_API_KEY`.
///
/// # Errors
/// [`SearchError::NoApiKey`] if no key is configured; otherwise forwards
/// HTTP/parse/vault errors.
pub async fn search(query: &str, count: usize) -> Result<Vec<SearchHit>, SearchError> {
    let vault = KeyVault::detect().map_err(|e| SearchError::Vault(e.to_string()))?;
    let key = resolve_api_key(&vault).await?;
    search_with_endpoint(TAVILY_ENDPOINT, query, SearchOptions { api_key: key, count }).await
}

/// Test-visible variant: same logic, custom endpoint.
///
/// # Errors
/// Forwards HTTP/parse errors from the upstream.
pub async fn search_with_endpoint(
    endpoint: &str,
    query: &str,
    opts: SearchOptions,
) -> Result<Vec<SearchHit>, SearchError> {
    let client = reqwest::Client::new();
    let resp = client
        .post(endpoint)
        .json(&ReqBody {
            api_key: &opts.api_key,
            query,
            max_results: opts.count,
            include_answer: true,
            search_depth: "basic",
        })
        .send()
        .await?;
    let raw: RawResp = resp.json().await.map_err(|e| SearchError::Parse(e.to_string()))?;
    Ok(hits_from_raw(raw))
}

/// Map a Tavily response to [`SearchHit`]s, prepending the grounded answer (when
/// present) as the first hit so the model reads it before the raw snippets.
fn hits_from_raw(raw: RawResp) -> Vec<SearchHit> {
    let mut hits: Vec<SearchHit> = raw
        .results
        .into_iter()
        .map(|h| SearchHit {
            title: h.title,
            url: h.url,
            snippet: h.content,
        })
        .collect();
    if let Some(answer) = raw.answer.filter(|a| !a.trim().is_empty()) {
        hits.insert(
            0,
            SearchHit {
                title: "Answer (grounded by Tavily)".to_string(),
                url: String::new(),
                snippet: answer,
            },
        );
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use origin_keyvault::Secret;
    use tokio::sync::Mutex;

    // All three resolve tests mutate the same process-global TAVILY_API_KEY
    // env var. cargo test runs them in parallel by default, so without
    // serialization they race. A tokio Mutex is async-aware and safe to
    // hold across awaits (unlike std::sync::Mutex, which clippy flags).
    static ENV_LOCK: Mutex<()> = Mutex::const_new(());

    #[test]
    fn grounded_answer_is_prepended_as_first_hit() {
        let raw = RawResp {
            results: vec![RawHit {
                title: "T".into(),
                url: "u".into(),
                content: "c".into(),
            }],
            answer: Some("the synthesized answer".into()),
        };
        let hits = hits_from_raw(raw);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Answer (grounded by Tavily)");
        assert_eq!(hits[0].snippet, "the synthesized answer");
        assert_eq!(hits[1].title, "T");
    }

    #[test]
    fn absent_or_blank_answer_leaves_results_unchanged() {
        for answer in [None, Some(String::new()), Some("   ".to_string())] {
            let raw = RawResp {
                results: vec![RawHit {
                    title: "T".into(),
                    url: "u".into(),
                    content: "c".into(),
                }],
                answer,
            };
            let hits = hits_from_raw(raw);
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].title, "T");
        }
    }

    #[tokio::test]
    async fn resolve_returns_vault_value_when_present() {
        let _g = ENV_LOCK.lock().await;
        let vault = KeyVault::in_memory();
        vault
            .set("tavily", "default", Secret::new("tvly-from-vault".to_string()))
            .await
            .expect("set");
        // Env var also present — vault must win.
        std::env::set_var("TAVILY_API_KEY", "tvly-from-env");
        let key = resolve_api_key(&vault).await.expect("resolve");
        assert_eq!(key, "tvly-from-vault");
        std::env::remove_var("TAVILY_API_KEY");
    }

    #[tokio::test]
    async fn resolve_falls_back_to_env_when_vault_empty() {
        let _g = ENV_LOCK.lock().await;
        let vault = KeyVault::in_memory();
        std::env::set_var("TAVILY_API_KEY", "tvly-env-only");
        let key = resolve_api_key(&vault).await.expect("resolve");
        assert_eq!(key, "tvly-env-only");
        std::env::remove_var("TAVILY_API_KEY");
    }

    #[tokio::test]
    async fn resolve_errors_when_neither_vault_nor_env_has_key() {
        let _g = ENV_LOCK.lock().await;
        let vault = KeyVault::in_memory();
        std::env::remove_var("TAVILY_API_KEY");
        let err = resolve_api_key(&vault).await.expect_err("must fail");
        assert!(matches!(err, SearchError::NoApiKey), "got {err:?}");
        // Message should guide the user to the fix.
        let msg = format!("{err}");
        assert!(
            msg.contains("tavily:default") || msg.contains("origin init"),
            "got {msg}"
        );
    }
}
