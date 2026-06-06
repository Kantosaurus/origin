// SPDX-License-Identifier: Apache-2.0
//! `WebFetch` tool — GET one or more URLs and return reader-mode markdown.
//!
//! Backward-compatible with single-URL calls (`{"url": "..."}`) while also
//! accepting a `urls` array (`{"urls": ["...", ...]}`), mirroring Gemini
//! `web_fetch` `urlContext`. With exactly one URL the output is the bare
//! markdown (unchanged); with several, sections are joined under `## <url>`
//! headers via [`origin_browser::web_fetch::render_combined_markdown`].
#![allow(clippy::module_name_repetitions)] // `web_fetch_args`/`web_fetch_many` are the documented entry points callers expect.

use crate::{SideEffects, Tier, Urgency};
use origin_browser::web_fetch::{fetch, fetch_many, render_combined_markdown, FetchOptions};
use serde_json::Value;

/// Fetch `url` and return markdown of the readable content.
///
/// # Errors
/// Returns a `String` describing HTTP, size, or parse failure.
pub async fn web_fetch(url: &str) -> Result<String, String> {
    let r = fetch(url, FetchOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    Ok(r.markdown)
}

/// Extract the requested URL list from a `WebFetch` arguments object.
///
/// Accepts either a `urls` array (preferred when present) or a single `url`
/// string. String entries in `urls` are kept; non-string entries are skipped.
/// Returns an empty `Vec` when neither field yields a usable URL — the caller
/// decides how to report the missing-argument case.
///
/// Pure (no I/O) so it can be unit-tested directly.
#[must_use]
pub fn parse_urls(args: &Value) -> Vec<String> {
    if let Some(arr) = args.get("urls").and_then(Value::as_array) {
        return arr.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect();
    }
    args.get("url")
        .and_then(Value::as_str)
        .map(|s| vec![s.to_owned()])
        .unwrap_or_default()
}

/// Fetch one or more URLs and return combined reader-mode markdown.
///
/// `urls` may contain up to [`MAX_URLS`](origin_browser::web_fetch::MAX_URLS)
/// entries; extras are dropped with a leading note (never silently). When the
/// list holds exactly one URL the result is the bare
/// markdown, identical to [`web_fetch`]. Per-URL failures are reported inline
/// under their section header rather than failing the whole call.
///
/// # Errors
/// Returns `Err` only when `urls` is empty (nothing to fetch). Individual
/// fetch failures are folded into the returned markdown, not propagated.
pub async fn web_fetch_many(urls: &[String]) -> Result<String, String> {
    match urls {
        [] => Err("WebFetch: no URL provided".to_owned()),
        // Exactly one URL: preserve the legacy bare-markdown shape, including
        // surfacing the fetch error to the caller (no per-URL section wrap).
        [single] => web_fetch(single).await,
        _ => {
            let (outcomes, truncated) = fetch_many(urls, &FetchOptions::default()).await;
            Ok(render_combined_markdown(&outcomes, truncated))
        }
    }
}

/// Parse `WebFetch` arguments (`url` and/or `urls`) and fetch accordingly.
///
/// Thin convenience over [`parse_urls`] + [`web_fetch_many`] for callers that
/// hold the raw arguments object.
///
/// # Errors
/// Returns `Err` when neither `url` nor a non-empty `urls` array is present.
pub async fn web_fetch_args(args: &Value) -> Result<String, String> {
    let urls = parse_urls(args);
    web_fetch_many(&urls).await
}

crate::origin_tool! {
    name: "WebFetch",
    description: "Fetch one or more URLs and return reader-mode markdown. Pass a single `url` string, or a `urls` array (up to 20) to fetch several at once; multi-URL output is sectioned under `## <url>` headers.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{"type":"object","properties":{"url":{"type":"string","description":"A single URL to fetch."},"urls":{"type":"array","items":{"type":"string"},"maxItems":20,"description":"Up to 20 URLs to fetch in one call; takes precedence over `url` when present."}}}"#,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_single_url_field() {
        let urls = parse_urls(&json!({ "url": "https://a.test" }));
        assert_eq!(urls, vec!["https://a.test".to_owned()]);
    }

    #[test]
    fn parse_urls_array() {
        let urls = parse_urls(&json!({ "urls": ["https://a.test", "https://b.test"] }));
        assert_eq!(
            urls,
            vec!["https://a.test".to_owned(), "https://b.test".to_owned()]
        );
    }

    #[test]
    fn urls_array_takes_precedence_over_url() {
        // When both are present, `urls` wins.
        let urls = parse_urls(&json!({
            "url": "https://ignored.test",
            "urls": ["https://a.test"]
        }));
        assert_eq!(urls, vec!["https://a.test".to_owned()]);
    }

    #[test]
    fn parse_skips_non_string_entries() {
        let urls = parse_urls(&json!({ "urls": ["https://a.test", 42, null, "https://b.test"] }));
        assert_eq!(
            urls,
            vec!["https://a.test".to_owned(), "https://b.test".to_owned()]
        );
    }

    #[test]
    fn parse_missing_both_is_empty() {
        assert!(parse_urls(&json!({})).is_empty());
    }

    #[test]
    fn empty_urls_returns_err() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build runtime");
        let res = rt.block_on(web_fetch_many(&[]));
        assert!(res.is_err());
    }

    #[test]
    fn parse_then_cap_drops_extras_with_note() {
        // End-to-end of the *pure* path (no network): build a 21-URL args
        // object, parse it, apply the cap, and render the combined markdown
        // over synthetic outcomes. Asserts the 21 -> 20 + note behavior and
        // per-URL section formatting without any fetch.
        use origin_browser::web_fetch::{cap_urls, UrlOutcome, MAX_URLS};

        let supplied: Vec<String> = (0..21).map(|i| format!("https://h{i}.test")).collect();
        let args = json!({ "urls": supplied });
        let parsed = parse_urls(&args);
        assert_eq!(parsed.len(), 21);

        let (capped, truncated) = cap_urls(&parsed);
        assert_eq!(capped.len(), MAX_URLS);
        assert_eq!(truncated, Some(21));

        let outcomes: Vec<UrlOutcome> = capped
            .iter()
            .map(|u| UrlOutcome {
                url: u.clone(),
                result: Ok("body".to_owned()),
            })
            .collect();
        let out = render_combined_markdown(&outcomes, truncated);

        assert!(
            out.contains(&format!("21 URLs supplied; fetched the first {MAX_URLS}")),
            "missing truncation note: {out}"
        );
        // The 21st URL (index 20) was dropped; the first is present.
        assert!(
            !out.contains("## https://h20.test"),
            "21st URL should be dropped: {out}"
        );
        assert!(out.contains("## https://h0.test"), "first URL missing: {out}");
        assert_eq!(out.matches("## https://h").count(), MAX_URLS);
    }
}
