// SPDX-License-Identifier: Apache-2.0
//! `WebFetch`: GET a URL, run readability over the HTML, return markdown.
//!
//! Pure Rust path — no subprocess required. Used for one-shot reader-mode
//! summaries where the router's snapshot/ref protocol would be overkill.

use std::fmt::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct FetchOptions {
    pub timeout: Duration,
    pub max_bytes: usize,
    pub user_agent: String,
    /// Permit loopback/private/link-local targets (see [`is_non_public_ip`]).
    /// Defaults to `ORIGIN_WEBFETCH_ALLOW_PRIVATE=1`, for local dev servers.
    pub allow_private: bool,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_bytes: 10 * 1024 * 1024,
            user_agent: concat!("origin/", env!("CARGO_PKG_VERSION")).into(),
            allow_private: std::env::var("ORIGIN_WEBFETCH_ALLOW_PRIVATE").is_ok_and(|v| v == "1"),
        }
    }
}

/// Whether `WebFetch` must refuse to contact `ip`.
///
/// Refused: loopback, link-local (incl. the 169.254.169.254 cloud-metadata
/// endpoint), private, unique-local (`fc00::/7`) and unspecified (`0.0.0.0/8`,
/// `::`), including IPv4 wrapped in IPv4-mapped IPv6.
#[must_use]
pub fn is_non_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.octets()[0] == 0,
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or_else(
            || {
                let head = v6.segments()[0];
                // fc00::/7 unique-local, fe80::/10 link-local (std helpers exceed the MSRV).
                v6.is_loopback() || v6.is_unspecified() || head & 0xfe00 == 0xfc00 || head & 0xffc0 == 0xfe80
            },
            |v4| is_non_public_ip(IpAddr::V4(v4)),
        ),
    }
}

/// Refusal reason when `url`'s host is a literal non-public IP. Literal IPs are
/// dialled without DNS, so [`PublicOnlyResolver`] never sees them; hostnames
/// are vetted there instead, after resolution.
fn literal_host_refusal(url: &url::Url) -> Option<String> {
    let ip = match url.host()? {
        url::Host::Ipv4(v4) => IpAddr::V4(v4),
        url::Host::Ipv6(v6) => IpAddr::V6(v6),
        url::Host::Domain(_) => return None,
    };
    is_non_public_ip(ip)
        .then(|| format!("{ip} is not a public address (set ORIGIN_WEBFETCH_ALLOW_PRIVATE=1 to allow)"))
}

/// DNS resolver that fails the lookup when ANY resolved address is non-public,
/// so a hostname cannot point the fetch at an internal service. Installed on
/// the client, it runs for every connection, including each redirect hop.
struct PublicOnlyResolver;

impl reqwest::dns::Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            if let Some(bad) = addrs.iter().find(|a| is_non_public_ip(a.ip())) {
                return Err(format!(
                    "{host} resolves to non-public address {} (set ORIGIN_WEBFETCH_ALLOW_PRIVATE=1 to allow)",
                    bad.ip()
                )
                .into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
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
    #[error("refused: {0}")]
    Blocked(String),
}

/// Fetch `url` and return reader-mode markdown.
///
/// # Errors
/// Returns [`FetchError`] for HTTP failure, oversized body, or readability
/// failure; [`FetchError::Blocked`] (or an HTTP error, for a hostname that
/// resolves to one) for a non-public target unless `opts.allow_private`.
pub async fn fetch(url: &str, opts: FetchOptions) -> Result<FetchResult, FetchError> {
    let guard = !opts.allow_private;
    if guard {
        if let Some(why) = literal_host_refusal(&url::Url::parse(url)?) {
            return Err(FetchError::Blocked(why));
        }
    }
    let mut builder = reqwest::Client::builder();
    if guard {
        // Every dial goes through the vetting resolver. A proxy would resolve
        // the target itself, unvetted, so proxies are bypassed while guarded.
        builder = builder.dns_resolver(Arc::new(PublicOnlyResolver)).no_proxy();
    }
    let client = builder
        .timeout(opts.timeout)
        .user_agent(&opts.user_agent)
        // Only follow redirects that stay on the original request's host. The
        // caller authorizes the *initial* URL against the domain allow-list;
        // without this, an allow-listed page could 3xx-bounce the fetch to an
        // arbitrary host (an internal service, a cloud metadata endpoint, an
        // exfil sink), escaping the allow-list. Cross-host redirects are
        // stopped rather than followed.
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            let origin_host = attempt.previous().first().and_then(|u| u.host_str());
            let next_host = attempt.url().host_str();
            if attempt.previous().len() > 10 {
                attempt.error("too many redirects")
            } else if let Some(why) = guard.then(|| literal_host_refusal(attempt.url())).flatten() {
                attempt.error(why)
            } else if origin_host.is_none() || origin_host == next_host {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
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

/// Maximum number of URLs accepted by a single multi-URL fetch.
///
/// Mirrors the Gemini `web_fetch` `urlContext` cap. Callers that supply more
/// than this should take the first [`MAX_URLS`] and surface a note rather than
/// silently dropping the rest (see [`fetch_many`]).
pub const MAX_URLS: usize = 20;

/// Outcome of a single URL within a [`fetch_many`] batch.
#[derive(Debug, Clone)]
pub struct UrlOutcome {
    /// The requested URL (not the post-redirect `final_url`).
    pub url: String,
    /// `Ok(markdown)` on success, `Err(reason)` on a per-URL failure.
    pub result: Result<String, String>,
}

/// Render a batch of [`UrlOutcome`]s as combined markdown.
///
/// Each URL becomes a `## <url>` section. Successful fetches contribute their
/// markdown; failures contribute `(fetch failed: <reason>)` so one bad URL
/// never aborts the whole call. When `truncated` is `Some(supplied)` a leading
/// note records that only [`MAX_URLS`] of `supplied` URLs were fetched.
///
/// This is a pure function (no I/O) so it can be unit-tested without network.
#[must_use]
pub fn render_combined_markdown(outcomes: &[UrlOutcome], truncated: Option<usize>) -> String {
    // Pre-size generously: header + a chunk per outcome.
    let mut out = String::with_capacity(64 + outcomes.len() * 256);
    if let Some(supplied) = truncated {
        let _ = write!(
            out,
            "_Note: {supplied} URLs supplied; fetched the first {MAX_URLS}._\n\n"
        );
    }
    for (i, oc) in outcomes.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n");
        }
        out.push_str("## ");
        out.push_str(&oc.url);
        out.push_str("\n\n");
        match &oc.result {
            Ok(md) => out.push_str(md),
            Err(reason) => {
                let _ = write!(out, "(fetch failed: {reason})");
            }
        }
    }
    out
}

/// Apply the [`MAX_URLS`] cap to a requested URL list.
///
/// Returns the capped slice (first [`MAX_URLS`] entries) and, when the input
/// exceeded the cap, `Some(original_len)` so callers can surface a note rather
/// than dropping silently. Pure — unit-testable without I/O.
#[must_use]
pub fn cap_urls(urls: &[String]) -> (&[String], Option<usize>) {
    let supplied = urls.len();
    let truncated = (supplied > MAX_URLS).then_some(supplied);
    (&urls[..supplied.min(MAX_URLS)], truncated)
}

/// Fetch every URL in `urls` and return their per-URL [`UrlOutcome`]s plus an
/// optional `truncated` count.
///
/// Inputs over [`MAX_URLS`] are capped to the first [`MAX_URLS`]; the returned
/// `Option<usize>` carries the original count so the caller can note it. Each
/// URL is fetched via the single-URL [`fetch`] path; a per-URL failure is
/// captured as `Err(reason)` rather than aborting the batch. Fetches run
/// sequentially — the agent loop is sequential within a turn, so this keeps
/// resource use predictable and avoids any new concurrency dependency.
pub async fn fetch_many(urls: &[String], opts: &FetchOptions) -> (Vec<UrlOutcome>, Option<usize>) {
    let (capped, truncated) = cap_urls(urls);
    let mut outcomes = Vec::with_capacity(capped.len());
    for url in capped {
        let result = match fetch(url, opts.clone()).await {
            Ok(r) => Ok(r.markdown),
            Err(e) => Err(e.to_string()),
        };
        outcomes.push(UrlOutcome {
            url: url.clone(),
            result,
        });
    }
    (outcomes, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(url: &str, md: &str) -> UrlOutcome {
        UrlOutcome {
            url: url.into(),
            result: Ok(md.into()),
        }
    }

    #[test]
    fn non_public_addresses_are_classified() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "::1",
            "::",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
        ] {
            assert!(is_non_public_ip(ip.parse().expect("ip")), "{ip} must be blocked");
        }
        for ip in ["93.184.216.34", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(!is_non_public_ip(ip.parse().expect("ip")), "{ip} is public");
        }
    }

    /// A9b: literal non-public hosts are refused before any connection.
    #[tokio::test]
    async fn literal_non_public_hosts_are_refused() {
        let opts = FetchOptions {
            allow_private: false,
            ..FetchOptions::default()
        };
        for url in ["http://127.0.0.1/", "http://169.254.169.254/", "http://[::1]/"] {
            let res = fetch(url, opts.clone()).await;
            assert!(matches!(res, Err(FetchError::Blocked(_))), "{url}: {res:?}");
        }
    }

    #[test]
    fn single_outcome_has_url_header_then_markdown() {
        let out = render_combined_markdown(&[ok("https://a.test", "Hello world.")], None);
        assert_eq!(out, "## https://a.test\n\nHello world.");
    }

    #[test]
    fn multiple_outcomes_are_separated_by_blank_line() {
        let out = render_combined_markdown(
            &[ok("https://a.test", "Body A."), ok("https://b.test", "Body B.")],
            None,
        );
        assert_eq!(
            out,
            "## https://a.test\n\nBody A.\n\n## https://b.test\n\nBody B."
        );
    }

    #[test]
    fn per_url_failure_renders_inline_without_aborting() {
        let outcomes = [
            ok("https://a.test", "Body A."),
            UrlOutcome {
                url: "https://bad.test".into(),
                result: Err("http: timed out".into()),
            },
        ];
        let out = render_combined_markdown(&outcomes, None);
        assert!(out.contains("## https://bad.test\n\n(fetch failed: http: timed out)"));
        assert!(out.contains("Body A."));
    }

    #[test]
    fn truncation_note_records_supplied_count() {
        let out = render_combined_markdown(&[ok("https://a.test", "x")], Some(21));
        assert!(out.starts_with("_Note: 21 URLs supplied; fetched the first 20._\n\n"));
        assert!(out.contains("## https://a.test"));
    }

    #[test]
    fn cap_under_limit_is_passthrough_no_note() {
        let urls: Vec<String> = (0..3).map(|i| format!("https://h{i}.test")).collect();
        let (capped, truncated) = cap_urls(&urls);
        assert_eq!(capped.len(), 3);
        assert!(truncated.is_none());
    }

    #[test]
    fn cap_at_limit_no_note() {
        let urls: Vec<String> = (0..MAX_URLS).map(|i| format!("https://h{i}.test")).collect();
        let (capped, truncated) = cap_urls(&urls);
        assert_eq!(capped.len(), MAX_URLS);
        assert!(truncated.is_none());
    }

    #[test]
    fn cap_over_limit_keeps_first_max_and_reports_supplied() {
        let urls: Vec<String> = (0..=MAX_URLS).map(|i| format!("https://h{i}.test")).collect();
        let (capped, truncated) = cap_urls(&urls);
        assert_eq!(capped.len(), MAX_URLS);
        assert_eq!(truncated, Some(MAX_URLS + 1));
        // The first MAX_URLS are retained in order; the extra is dropped.
        assert_eq!(capped.first().map(String::as_str), Some("https://h0.test"));
        assert_eq!(
            capped.last().map(String::as_str),
            Some(format!("https://h{}.test", MAX_URLS - 1).as_str())
        );
    }
}
