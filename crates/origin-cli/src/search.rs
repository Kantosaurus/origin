// SPDX-License-Identifier: Apache-2.0
//! `origin search <query>` — pluggable web search.
//!
//! Fetches results from `DuckDuckGo` (no key), Brave, or Tavily and prints the
//! ranked hits. Endpoint construction, response parsing, and ranking all live
//! in the pure [`origin_websearch`] crate (openclaude free `DuckDuckGo` +
//! multi-adapter search); this module only performs the live HTTP fetch via the
//! crate's existing `reqwest` dependency.

use anyhow::Result;
use origin_websearch::{
    endpoint_for, ground, parse_brave_json, parse_duckduckgo_html, parse_tavily_json, rank, Engine,
    SearchHit,
};

/// Parse the `--engine` flag into an [`Engine`], defaulting to `DuckDuckGo`.
fn parse_engine(engine: Option<&str>) -> Engine {
    match engine.map(str::to_ascii_lowercase).as_deref() {
        Some("brave") => Engine::Brave,
        Some("tavily") => Engine::Tavily,
        _ => Engine::DuckDuckGo,
    }
}

/// Resolve the API key (if any) for `engine` from the environment.
fn key_for(engine: Engine) -> Option<String> {
    match engine {
        Engine::Brave => std::env::var("ORIGIN_BRAVE_KEY").ok(),
        Engine::Tavily => std::env::var("ORIGIN_TAVILY_KEY").ok(),
        Engine::DuckDuckGo => None,
    }
}

/// Run `origin search`: fetch, parse, rank, and print the top hits.
///
/// Brave/Tavily fall back to `DuckDuckGo` when their key is missing. Network
/// failures are surfaced as an error but never panic.
///
/// # Errors
/// Returns when the HTTP client cannot be built or the request fails.
pub async fn run(query: &str, engine: Option<String>) -> Result<()> {
    let mut engine = parse_engine(engine.as_deref());
    let mut key = key_for(engine);

    // Brave / Tavily require a key; fall back to DuckDuckGo when one is absent.
    if matches!(engine, Engine::Brave | Engine::Tavily) && key.is_none() {
        eprintln!("no API key for selected engine; falling back to DuckDuckGo");
        engine = Engine::DuckDuckGo;
        key = None;
    }

    let (url, headers) = endpoint_for(engine, query, key.as_deref());

    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| anyhow::anyhow!("building HTTP client: {e}"))?;

    let mut header_map = reqwest::header::HeaderMap::new();
    for (name, value) in &headers {
        if let (Ok(n), Ok(v)) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            header_map.insert(n, v);
        }
    }

    let request = if matches!(engine, Engine::Tavily) {
        let body = serde_json::json!({ "api_key": key.unwrap_or_default(), "query": query });
        client.post(&url).headers(header_map).json(&body)
    } else {
        client.get(&url).headers(header_map)
    };

    let response = request
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("search request failed: {e}"))?;
    let body = response
        .text()
        .await
        .map_err(|e| anyhow::anyhow!("reading search response: {e}"))?;

    let hits: Vec<SearchHit> = match engine {
        Engine::DuckDuckGo => parse_duckduckgo_html(&body),
        Engine::Brave => parse_brave_json(&body).map_err(|e| anyhow::anyhow!("{e}"))?,
        Engine::Tavily => parse_tavily_json(&body).map_err(|e| anyhow::anyhow!("{e}"))?,
    };

    let ranked = rank(hits, query);
    if ranked.is_empty() {
        println!("no results");
        return Ok(());
    }
    for hit in &ranked {
        println!("{}", hit.title);
        println!("  {}", hit.url);
        if !hit.snippet.is_empty() {
            println!("  {}", hit.snippet);
        }
    }

    // Opt-in grounded, cited summary over the top ranked hits. Offline
    // synthesis from the snippets already fetched — no extra network call.
    // Unset ⇒ output is byte-identical to the prior link-list behavior.
    if std::env::var_os("ORIGIN_SEARCH_GROUND").is_some() {
        let grounded = ground(query, &ranked);
        if !grounded.is_empty() {
            println!("\n{}", grounded.render());
        }
    }
    Ok(())
}
