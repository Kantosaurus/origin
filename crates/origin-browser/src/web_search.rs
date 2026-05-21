//! WebSearch via Tavily.
//!
//! Tavily was chosen over Brave/SerpAPI for LLM-friendly snippets and a
//! single-endpoint API. The function takes the endpoint as a parameter in
//! the `_with_endpoint` variant so tests can point at a local server.

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
    #[error("TAVILY_API_KEY is not set")]
    NoApiKey,
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("parse: {0}")]
    Parse(String),
}

#[derive(Serialize)]
struct ReqBody<'a> { api_key: &'a str, query: &'a str, max_results: usize }
#[derive(Deserialize)]
struct RawResp { results: Vec<RawHit> }
#[derive(Deserialize)]
struct RawHit { title: String, url: String, content: String }

/// Search Tavily; reads `TAVILY_API_KEY` from env.
///
/// # Errors
/// [`SearchError::NoApiKey`] if env unset, otherwise forwards HTTP/parse errors.
pub async fn search(query: &str, count: usize) -> Result<Vec<SearchHit>, SearchError> {
    let key = std::env::var("TAVILY_API_KEY").map_err(|_| SearchError::NoApiKey)?;
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
        .json(&ReqBody { api_key: &opts.api_key, query, max_results: opts.count })
        .send()
        .await?;
    let raw: RawResp = resp.json().await.map_err(|e| SearchError::Parse(e.to_string()))?;
    Ok(raw.results.into_iter().map(|h| SearchHit {
        title: h.title, url: h.url, snippet: h.content,
    }).collect())
}
