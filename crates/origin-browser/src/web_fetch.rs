//! `WebFetch`: GET a URL, run readability over the HTML, return markdown.
//!
//! Pure Rust path — no subprocess required. Used for one-shot reader-mode
//! summaries where the router's snapshot/ref protocol would be overkill.

use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct FetchOptions {
    pub timeout: Duration,
    pub max_bytes: usize,
    pub user_agent: String,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_bytes: 10 * 1024 * 1024,
            user_agent: concat!("origin/", env!("CARGO_PKG_VERSION")).into(),
        }
    }
}

#[derive(Debug)]
pub struct FetchResult {
    pub final_url: String,
    pub content_type: String,
    pub markdown: String,
}

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("body too large: {0} bytes > limit {1}")]
    TooLarge(usize, usize),
    #[error("readability: {0}")]
    Readability(String),
    #[error("url parse: {0}")]
    Url(#[from] url::ParseError),
}

/// Fetch `url` and return reader-mode markdown.
///
/// # Errors
/// Returns [`FetchError`] for HTTP failure, oversized body, or readability
/// failure.
pub async fn fetch(url: &str, opts: FetchOptions) -> Result<FetchResult, FetchError> {
    let client = reqwest::Client::builder()
        .timeout(opts.timeout)
        .user_agent(&opts.user_agent)
        .build()?;
    let mut resp = client.get(url).send().await?;
    let final_url = resp.url().to_string();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    // Enforce `max_bytes` while streaming so an oversized (or chunked,
    // unknown-length) response can't be fully buffered into memory before the
    // size check — that defeats the limit and is a memory-DoS vector.
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if bytes.len() + chunk.len() > opts.max_bytes {
            return Err(FetchError::TooLarge(bytes.len() + chunk.len(), opts.max_bytes));
        }
        bytes.extend_from_slice(&chunk);
    }
    let html = String::from_utf8_lossy(&bytes).into_owned();
    let parsed_url = url::Url::parse(&final_url)?;
    let product = readability::extractor::extract(&mut html.as_bytes(), &parsed_url)
        .map_err(|e| FetchError::Readability(e.to_string()))?;
    let markdown = html2md::parse_html(&product.content);
    Ok(FetchResult {
        final_url,
        content_type,
        markdown,
    })
}
