// SPDX-License-Identifier: Apache-2.0
//! Pure request builders for the OAuth refresh exchange and the Gmail REST
//! endpoints.
//!
//! These functions construct URLs, query strings, and form bodies but perform
//! no I/O, so they are fully unit-testable. The HTTP layer ([`crate::http`])
//! feeds their output to `reqwest`.
//!
//! Token frugality is baked in here: `messages.get` defaults to
//! `format=metadata` with a tight `metadataHeaders` allow-list, and every
//! list call carries an explicit `maxResults` cap so we never over-fetch.

use crate::creds::Credentials;

/// Google's OAuth 2.0 token endpoint.
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Base URL for Gmail REST API v1, scoped to the authenticated user.
pub const GMAIL_BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

/// Default headers requested with `format=metadata`.
///
/// Keeping this list tight is the core token-frugality lever: we ask for
/// exactly the headers a triage view needs rather than the full RFC 822
/// header block.
pub const DEFAULT_METADATA_HEADERS: &[&str] = &["From", "To", "Cc", "Subject", "Date"];

/// Hard ceiling on `maxResults` for any single list call. Google itself caps
/// `messages.list` at 500; we mirror that so a caller cannot request an
/// unbounded page.
pub const MAX_PAGE: u32 = 500;

/// Build the `application/x-www-form-urlencoded` field pairs for an RFC 6749
/// §6 refresh-token grant against Google's token endpoint.
///
/// Returns owned key/value pairs (borrowing the credential strings) suitable
/// for `reqwest::RequestBuilder::form`. The values are bearer secrets — the
/// caller must not log the returned vector.
#[must_use]
pub fn refresh_form(creds: &Credentials) -> Vec<(&'static str, String)> {
    vec![
        ("grant_type", "refresh_token".to_owned()),
        ("refresh_token", creds.refresh_token().to_owned()),
        ("client_id", creds.client_id().to_owned()),
        ("client_secret", creds.client_secret().to_owned()),
    ]
}

/// Clamp a caller-supplied page size into `1..=MAX_PAGE`.
#[must_use]
pub fn clamp_max(max: u32) -> u32 {
    max.clamp(1, MAX_PAGE)
}

/// Percent-encode a query-string component (RFC 3986 unreserved set kept
/// literal; everything else `%`-escaped). Pure and allocation-light; avoids a
/// dependency just for `q=` encoding.
#[must_use]
pub fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(hex_digit(b >> 4));
                out.push(hex_digit(b & 0x0f));
            }
        }
    }
    out
}

const fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'A' + (nibble - 10)) as char,
    }
}

/// Build the full URL for a `messages.list` call.
///
/// `query` is a Gmail search expression (same syntax as the Gmail search box,
/// e.g. `from:alice is:unread newer_than:7d`). `max` is clamped via
/// [`clamp_max`]. `page_token` continues a previous page when `Some`.
#[must_use]
pub fn messages_list_url(query: &str, max: u32, page_token: Option<&str>) -> String {
    let mut url = format!(
        "{GMAIL_BASE}/messages?maxResults={}&q={}",
        clamp_max(max),
        encode_component(query)
    );
    if let Some(tok) = page_token {
        url.push_str("&pageToken=");
        url.push_str(&encode_component(tok));
    }
    url
}

/// Build the full URL for a `threads.list` call. See [`messages_list_url`].
#[must_use]
pub fn threads_list_url(query: &str, max: u32, page_token: Option<&str>) -> String {
    let mut url = format!(
        "{GMAIL_BASE}/threads?maxResults={}&q={}",
        clamp_max(max),
        encode_component(query)
    );
    if let Some(tok) = page_token {
        url.push_str("&pageToken=");
        url.push_str(&encode_component(tok));
    }
    url
}

/// Build the full URL for a token-frugal `messages.get` call: `format=metadata`
/// plus a repeated `metadataHeaders` allow-list. Pass an empty `headers` slice
/// to fall back to [`DEFAULT_METADATA_HEADERS`].
#[must_use]
pub fn message_get_metadata_url(id: &str, headers: &[&str]) -> String {
    let chosen = if headers.is_empty() {
        DEFAULT_METADATA_HEADERS
    } else {
        headers
    };
    let mut url = format!("{GMAIL_BASE}/messages/{}?format=metadata", encode_component(id));
    for h in chosen {
        url.push_str("&metadataHeaders=");
        url.push_str(&encode_component(h));
    }
    url
}

/// Build the full URL for a `format=full` `messages.get` call (decodes the
/// body). More expensive in tokens than the metadata form; used when a caller
/// explicitly asks for the body.
#[must_use]
pub fn message_get_full_url(id: &str) -> String {
    format!("{GMAIL_BASE}/messages/{}?format=full", encode_component(id))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sample_creds() -> Credentials {
        Credentials::from_json(r#"{"client_id":"cid","client_secret":"csecret","refresh_token":"rtok"}"#)
            .unwrap()
    }

    #[test]
    fn refresh_form_has_grant_and_all_fields() {
        let creds = sample_creds();
        let form = refresh_form(&creds);
        assert!(form.contains(&("grant_type", "refresh_token".to_owned())));
        assert!(form.contains(&("refresh_token", "rtok".to_owned())));
        assert!(form.contains(&("client_id", "cid".to_owned())));
        assert!(form.contains(&("client_secret", "csecret".to_owned())));
        assert_eq!(form.len(), 4);
    }

    #[test]
    fn clamp_max_bounds() {
        assert_eq!(clamp_max(0), 1);
        assert_eq!(clamp_max(50), 50);
        assert_eq!(clamp_max(10_000), MAX_PAGE);
    }

    #[test]
    fn encode_component_escapes_spaces_and_specials() {
        assert_eq!(encode_component("from:a b"), "from%3Aa%20b");
        assert_eq!(encode_component("a~b-c_d.e"), "a~b-c_d.e");
        assert_eq!(encode_component("100%"), "100%25");
    }

    #[test]
    fn messages_list_url_encodes_query_and_caps() {
        let url = messages_list_url("is:unread from:bob@x.com", 9999, None);
        assert!(url.starts_with(&format!("{GMAIL_BASE}/messages?maxResults={MAX_PAGE}&q=")));
        assert!(url.contains("is%3Aunread%20from%3Abob%40x.com"));
        assert!(!url.contains("pageToken"));
    }

    #[test]
    fn messages_list_url_appends_page_token() {
        let url = messages_list_url("x", 10, Some("TOK 1"));
        assert!(url.contains("&pageToken=TOK%201"));
        assert!(url.contains("maxResults=10"));
    }

    #[test]
    fn threads_list_url_shape() {
        let url = threads_list_url("label:work", 5, None);
        assert!(url.starts_with(&format!("{GMAIL_BASE}/threads?maxResults=5&q=")));
        assert!(url.contains("label%3Awork"));
    }

    #[test]
    fn message_get_metadata_default_headers() {
        let url = message_get_metadata_url("18c", &[]);
        assert!(url.starts_with(&format!("{GMAIL_BASE}/messages/18c?format=metadata")));
        for h in DEFAULT_METADATA_HEADERS {
            assert!(
                url.contains(&format!("metadataHeaders={h}")),
                "missing {h}: {url}"
            );
        }
    }

    #[test]
    fn message_get_metadata_custom_headers() {
        let url = message_get_metadata_url("18c", &["Subject"]);
        assert!(url.contains("metadataHeaders=Subject"));
        assert!(!url.contains("metadataHeaders=From"));
    }

    #[test]
    fn message_get_full_url_shape() {
        let url = message_get_full_url("xy z");
        assert_eq!(url, format!("{GMAIL_BASE}/messages/xy%20z?format=full"));
    }
}
