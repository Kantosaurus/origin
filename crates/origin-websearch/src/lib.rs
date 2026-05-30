// SPDX-License-Identifier: Apache-2.0
//! Pluggable web search with offline-testable result parsers.
//!
//! This crate provides `HTML` and `JSON` parsers for several search engines
//! plus a simple term-overlap reranker. The live network fetch is injected via
//! a closure or trait, so the entire crate is unit-testable offline.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can occur while parsing search-engine responses.
#[derive(Debug, Error)]
pub enum SearchError {
    /// A response body could not be parsed into search hits.
    #[error("parse error: {0}")]
    Parse(String),
}

/// A single search result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    /// The result title.
    pub title: String,
    /// The result address.
    pub url: String,
    /// A short snippet describing the result.
    pub snippet: String,
}

/// The supported search engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Engine {
    /// The `DuckDuckGo` `HTML` endpoint (no `API` key required).
    DuckDuckGo,
    /// The Brave Search `API` (requires an `API` key).
    Brave,
    /// The Tavily Search `API` (requires an `API` key).
    Tavily,
}

/// Parses the `DuckDuckGo` `HTML` endpoint into a list of hits.
///
/// The endpoint at `html.duckduckgo.com/html` returns each result as an anchor
/// of class `result__a` (title plus href) followed by an element of class
/// `result__snippet`. This parser scans for those markers without a full
/// `HTML` library, decoding the redirect link and basic `HTML` entities.
///
/// Malformed or empty input yields an empty vector rather than an error, since a
/// best-effort scrape should degrade gracefully.
#[must_use]
pub fn parse_duckduckgo_html(html: &str) -> Vec<SearchHit> {
    // Split on the result-anchor class marker; the first chunk is preamble.
    html.split("result__a").skip(1).filter_map(parse_ddg_chunk).collect()
}

/// Parses a single post-`result__a` chunk into a hit, if it has a usable link.
fn parse_ddg_chunk(part: &str) -> Option<SearchHit> {
    let href = extract_attr(part, "href")?;
    let url = decode_ddg_url(&href);
    if url.is_empty() {
        return None;
    }
    let title = extract_anchor_text(part).unwrap_or_default();
    let snippet = part
        .find("result__snippet")
        .and_then(|idx| extract_anchor_text(&part[idx..]))
        .unwrap_or_default();
    Some(SearchHit { title, url, snippet })
}

/// Parses a Brave Search `API` `JSON` response into a list of hits.
///
/// Brave returns `{ "web": { "results": [ { "title", "url", "description" } ] } }`.
///
/// # Errors
///
/// Returns [`SearchError::Parse`] if the body is not valid `JSON`.
pub fn parse_brave_json(json: &str) -> Result<Vec<SearchHit>, SearchError> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| SearchError::Parse(e.to_string()))?;
    let results = value
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(serde_json::Value::as_array);
    Ok(hits_from_array(results, "description"))
}

/// Parses a Tavily Search `API` `JSON` response into a list of hits.
///
/// Tavily returns `{ "results": [ { "title", "url", "content" } ] }`.
///
/// # Errors
///
/// Returns [`SearchError::Parse`] if the body is not valid `JSON`.
pub fn parse_tavily_json(json: &str) -> Result<Vec<SearchHit>, SearchError> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| SearchError::Parse(e.to_string()))?;
    let results = value.get("results").and_then(serde_json::Value::as_array);
    Ok(hits_from_array(results, "content"))
}

/// Builds hits from a `JSON` results array, reading the snippet from `snippet_key`.
fn hits_from_array(results: Option<&Vec<serde_json::Value>>, snippet_key: &str) -> Vec<SearchHit> {
    let Some(arr) = results else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            let url = json_str(item, "url");
            if url.is_empty() {
                return None;
            }
            Some(SearchHit {
                title: json_str(item, "title"),
                url,
                snippet: json_str(item, snippet_key),
            })
        })
        .collect()
}

/// Builds the request endpoint and header list for a given engine and query.
///
/// Returns the fully-formed request address plus a list of `(name, value)`
/// header pairs. The `DuckDuckGo` endpoint needs no `API` key; Brave and Tavily
/// attach the key as a header when one is supplied. The returned tuple is
/// intended to be fed to an injected fetcher.
// `Engine` is a tiny `Copy` enum and the public API takes it by value by design.
#[must_use]
#[allow(clippy::needless_pass_by_value)]
pub fn endpoint_for(
    engine: Engine,
    query: &str,
    api_key: Option<&str>,
) -> (String, Vec<(String, String)>) {
    let encoded = urlencode(query);
    match engine {
        Engine::DuckDuckGo => {
            let url = format!("https://html.duckduckgo.com/html/?q={encoded}");
            let headers = vec![("User-Agent".to_owned(), "Mozilla/5.0 (origin-websearch)".to_owned())];
            (url, headers)
        }
        Engine::Brave => {
            let url = format!("https://api.search.brave.com/res/v1/web/search?q={encoded}");
            let mut headers = vec![("Accept".to_owned(), "application/json".to_owned())];
            if let Some(key) = api_key {
                headers.push(("X-Subscription-Token".to_owned(), key.to_owned()));
            }
            (url, headers)
        }
        Engine::Tavily => {
            let url = "https://api.tavily.com/search".to_owned();
            let mut headers = vec![("Content-Type".to_owned(), "application/json".to_owned())];
            if let Some(key) = api_key {
                headers.push(("Authorization".to_owned(), format!("Bearer {key}")));
            }
            (url, headers)
        }
    }
}

/// Reranks hits by query-term overlap and removes duplicates by address.
///
/// Each hit is scored by the number of distinct lowercase query terms that
/// appear in its title or snippet. Hits are sorted by descending score (stable,
/// so the engine's original order breaks ties) and deduplicated by address,
/// keeping the first (highest-ranked) occurrence.
#[must_use]
pub fn rank(hits: Vec<SearchHit>, query: &str) -> Vec<SearchHit> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|t| !t.is_empty())
        .collect();
    let mut scored: Vec<(usize, SearchHit)> = hits
        .into_iter()
        .map(|hit| {
            let haystack = format!("{} {}", hit.title, hit.snippet).to_lowercase();
            let score = terms.iter().filter(|t| haystack.contains(t.as_str())).count();
            (score, hit)
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));

    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for (_, hit) in scored {
        if seen.iter().any(|u| u == &hit.url) {
            continue;
        }
        seen.push(hit.url.clone());
        out.push(hit);
    }
    out
}

/// Percent-encodes a query string for use in a `URL` query component.
fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            other => {
                use std::fmt::Write as _;
                // Writing to a String never fails; ignore the (impossible) error.
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// Reads a string field from a `JSON` object, defaulting to empty.
fn json_str(item: &serde_json::Value, key: &str) -> String {
    item.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Extracts the value of an `HTML` attribute (e.g. `href`) from a fragment.
fn extract_attr(fragment: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = fragment.find(&needle)? + needle.len();
    let rest = &fragment[start..];
    let end = rest.find('"')?;
    Some(decode_entities(&rest[..end]))
}

/// Extracts the inner text of the first anchor/tag close in a fragment.
fn extract_anchor_text(fragment: &str) -> Option<String> {
    // Move past the opening tag's closing '>'.
    let gt = fragment.find('>')?;
    let rest = &fragment[gt + 1..];
    let end = rest.find('<').unwrap_or(rest.len());
    let text = rest[..end].trim();
    if text.is_empty() {
        None
    } else {
        Some(decode_entities(text))
    }
}

/// Decodes a `DuckDuckGo` redirect link into a plain destination address.
///
/// `DuckDuckGo` wraps results as `//duckduckgo.com/l/?uddg=<encoded>&...`. This
/// unwraps the `uddg` parameter; non-wrapped links get a scheme prepended.
fn decode_ddg_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(idx) = trimmed.find("uddg=") {
        let after = &trimmed[idx + "uddg=".len()..];
        let encoded = after.split('&').next().unwrap_or(after);
        return percent_decode(encoded);
    }
    if let Some(stripped) = trimmed.strip_prefix("//") {
        return format!("https://{stripped}");
    }
    trimmed.to_owned()
}

/// Decodes a percent-encoded string (`UTF-8` best-effort).
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = hex_val(bytes[i + 1]);
                let lo = hex_val(bytes[i + 2]);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h << 4) | l);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Converts a single hex `ASCII` byte to its numeric value.
const fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Decodes the small set of `HTML` entities common in search snippets.
fn decode_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    const DDG_FIXTURE: &str = r#"
    <div class="results">
      <div class="result">
        <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F&amp;rut=abc">The Rust Programming Language</a>
        <a class="result__snippet">A language empowering everyone &amp; building reliable software.</a>
      </div>
      <div class="result">
        <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdoc.rust-lang.org%2Fbook%2F">The Rust Book</a>
        <a class="result__snippet">Learn Rust the official way.</a>
      </div>
    </div>
    "#;

    #[test]
    fn parses_ddg_fixture_into_hits() {
        let hits = parse_duckduckgo_html(DDG_FIXTURE);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://www.rust-lang.org/");
        assert_eq!(hits[0].title, "The Rust Programming Language");
        assert!(hits[0].snippet.contains("empowering everyone & building"));
        assert_eq!(hits[1].url, "https://doc.rust-lang.org/book/");
    }

    #[test]
    fn empty_html_yields_no_hits() {
        assert!(parse_duckduckgo_html("").is_empty());
        assert!(parse_duckduckgo_html("<html><body>nothing here</body></html>").is_empty());
    }

    #[test]
    fn parses_brave_json_fixture() {
        let json = r#"{
            "web": { "results": [
                { "title": "Brave One", "url": "https://a.example/", "description": "first" },
                { "title": "Brave Two", "url": "https://b.example/", "description": "second" }
            ] }
        }"#;
        let hits = parse_brave_json(json).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Brave One");
        assert_eq!(hits[1].url, "https://b.example/");
        assert_eq!(hits[0].snippet, "first");
    }

    #[test]
    fn parses_tavily_json_fixture() {
        let json = r#"{
            "results": [
                { "title": "Tav One", "url": "https://t1.example/", "content": "alpha" },
                { "title": "Tav Two", "url": "https://t2.example/", "content": "beta" }
            ]
        }"#;
        let hits = parse_tavily_json(json).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].snippet, "alpha");
        assert_eq!(hits[1].title, "Tav Two");
    }

    #[test]
    fn invalid_json_is_parse_error() {
        let err = parse_brave_json("{not json").unwrap_err();
        assert!(matches!(err, SearchError::Parse(_)));
        assert!(parse_tavily_json("nope").is_err());
    }

    #[test]
    fn brave_json_missing_results_is_empty() {
        let hits = parse_brave_json(r#"{"web":{}}"#).unwrap();
        assert!(hits.is_empty());
        let hits2 = parse_tavily_json(r"{}").unwrap();
        assert!(hits2.is_empty());
    }

    #[test]
    fn endpoint_ddg_has_no_key_header() {
        let (url, headers) = endpoint_for(Engine::DuckDuckGo, "rust lang", None);
        assert!(url.contains("html.duckduckgo.com"));
        assert!(url.contains("q=rust+lang"));
        assert!(headers.iter().all(|(k, _)| k != "X-Subscription-Token"));
        assert!(headers.iter().any(|(k, _)| k == "User-Agent"));
    }

    #[test]
    fn endpoint_brave_attaches_key_header() {
        let (url, headers) = endpoint_for(Engine::Brave, "a b", Some("SECRET"));
        assert!(url.starts_with("https://api.search.brave.com"));
        let token = headers.iter().find(|(k, _)| k == "X-Subscription-Token");
        assert_eq!(token.map(|(_, v)| v.as_str()), Some("SECRET"));
    }

    #[test]
    fn endpoint_tavily_attaches_bearer() {
        let (url, headers) = endpoint_for(Engine::Tavily, "q", Some("KEY"));
        assert!(url.contains("api.tavily.com"));
        let auth = headers.iter().find(|(k, _)| k == "Authorization");
        assert_eq!(auth.map(|(_, v)| v.as_str()), Some("Bearer KEY"));
    }

    #[test]
    fn rank_dedups_by_url_and_orders_by_overlap() {
        let hits = vec![
            SearchHit {
                title: "unrelated page".to_owned(),
                url: "https://low.example/".to_owned(),
                snippet: "nothing matching".to_owned(),
            },
            SearchHit {
                title: "rust async runtime".to_owned(),
                url: "https://high.example/".to_owned(),
                snippet: "rust async tokio".to_owned(),
            },
            SearchHit {
                title: "duplicate".to_owned(),
                url: "https://high.example/".to_owned(),
                snippet: "rust".to_owned(),
            },
        ];
        let ranked = rank(hits, "rust async");
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].url, "https://high.example/");
        assert_eq!(ranked[1].url, "https://low.example/");
    }

    #[test]
    fn rank_empty_query_preserves_order_and_dedups() {
        let hits = vec![
            SearchHit { title: "a".to_owned(), url: "https://x/".to_owned(), snippet: String::new() },
            SearchHit { title: "b".to_owned(), url: "https://x/".to_owned(), snippet: String::new() },
            SearchHit { title: "c".to_owned(), url: "https://y/".to_owned(), snippet: String::new() },
        ];
        let ranked = rank(hits, "   ");
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].url, "https://x/");
        assert_eq!(ranked[0].title, "a");
        assert_eq!(ranked[1].url, "https://y/");
    }
}
