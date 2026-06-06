// SPDX-License-Identifier: Apache-2.0
//! HTTP seam: the only part of the crate that touches the network.
//!
//! Everything network-bound funnels through [`HttpClient`]; the request URLs
//! and bodies come from [`crate::request`] (pure) and the response bytes go to
//! [`crate::model`] (pure). The token-response parser [`parse_token_response`]
//! is itself pure and unit-tested.
//!
//! TLS: backed by the workspace `reqwest` (default-features off, `rustls-tls`).
//! No OpenSSL / native-tls is ever pulled in.

use origin_keyvault::Secret;
use serde::Deserialize;

use crate::error::{Error, Result};

/// A thin wrapper over `reqwest::Client` carrying a bearer access token.
///
/// The access token is held as [`Secret<String>`] so it zeroizes on drop and
/// never appears in `Debug`. The struct derives no `Debug` of its own to keep
/// the token off any log surface.
#[allow(clippy::module_name_repetitions)] // `HttpClient` is the documented seam type Phase 2 consumes.
pub struct HttpClient {
    inner: reqwest::Client,
    access_token: Secret<String>,
}

impl HttpClient {
    /// Wrap an existing `reqwest::Client` with a bearer access token.
    #[must_use]
    pub const fn new(inner: reqwest::Client, access_token: Secret<String>) -> Self {
        Self { inner, access_token }
    }

    /// Perform an authenticated `GET` and return the response body as a
    /// `String`. Non-2xx responses become [`Error::Status`].
    ///
    /// # Errors
    /// [`Error::Http`] on transport failure, [`Error::Status`] on non-2xx.
    pub async fn get(&self, url: &str) -> Result<String> {
        let resp = self
            .inner
            .get(url)
            .bearer_auth(self.access_token.expose())
            .send()
            .await
            .map_err(|e| Error::Http(redact_url(&e.to_string())))?;
        read_checked(resp).await
    }
}

/// Read a response, mapping non-2xx into [`Error::Status`] and 2xx into the
/// body text. Shared by [`HttpClient::get`] and the token exchange.
async fn read_checked(resp: reqwest::Response) -> Result<String> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| Error::Http(format!("read body: {e}")))?;
    if status.is_success() {
        Ok(body)
    } else {
        Err(Error::status(status.as_u16(), body))
    }
}

/// Best-effort scrub of any `access_token=`/`refresh_token=` query fragment a
/// transport error string might echo back. Defence-in-depth: reqwest errors
/// normally only carry the host, but we never want a token in a log line.
fn redact_url(msg: &str) -> String {
    let mut out = msg.to_owned();
    for needle in ["access_token=", "refresh_token=", "client_secret="] {
        if let Some(pos) = out.find(needle) {
            out.truncate(pos + needle.len());
            out.push_str("<redacted>");
        }
    }
    out
}

/// Successful `/token` response. No `Debug`: `access_token` is a bearer secret
/// and the `xtask lint-secrets` gate forbids deriving `Debug` over it.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: u64,
}

/// Outcome of a refresh exchange. The access token is wrapped so it cannot
/// leak through `Debug`.
#[derive(Debug)]
#[non_exhaustive]
pub struct RefreshedToken {
    /// Freshly minted bearer access token.
    pub access_token: Secret<String>,
    /// Lifetime in seconds reported by Google (`0` if absent).
    pub expires_in: u64,
}

/// Parse a Google `/token` JSON response body into a [`RefreshedToken`].
///
/// Pure (no I/O) so the success path is unit-tested without a network round
/// trip. The HTTP send that produced `body` is the only un-tested seam.
///
/// # Errors
/// Returns [`Error::Parse`] if `body` is not a valid token response.
pub fn parse_token_response(body: &str) -> Result<RefreshedToken> {
    let raw: TokenResponse = serde_json::from_str(body).map_err(|e| Error::Parse(e.to_string()))?;
    Ok(RefreshedToken {
        access_token: Secret::new(raw.access_token),
        expires_in: raw.expires_in,
    })
}

/// Exchange a refresh-token form for a fresh access token against Google's
/// `/token` endpoint. The `form` comes from [`crate::request::refresh_form`]
/// and carries bearer secrets — it must not be logged.
///
/// # Errors
/// [`Error::Http`] on transport failure, [`Error::Status`] on non-2xx,
/// [`Error::Parse`] if the success body is not a token response.
pub async fn exchange_refresh(
    client: &reqwest::Client,
    token_url: &str,
    form: &[(&'static str, String)],
) -> Result<RefreshedToken> {
    let resp = client
        .post(token_url)
        .form(form)
        .send()
        .await
        .map_err(|e| Error::Http(redact_url(&e.to_string())))?;
    let body = read_checked(resp).await?;
    parse_token_response(&body)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_token_response_happy_path() {
        let body = r#"{"access_token":"ya29.fresh","expires_in":3599,"token_type":"Bearer"}"#;
        let t = parse_token_response(body).unwrap();
        assert_eq!(t.access_token.expose(), "ya29.fresh");
        assert_eq!(t.expires_in, 3599);
    }

    #[test]
    fn parse_token_response_missing_expiry_defaults_zero() {
        let t = parse_token_response(r#"{"access_token":"x"}"#).unwrap();
        assert_eq!(t.expires_in, 0);
    }

    #[test]
    fn parse_token_response_bad_json_errors() {
        assert!(matches!(
            parse_token_response("oops").unwrap_err(),
            Error::Parse(_)
        ));
    }

    #[test]
    fn refreshed_token_debug_redacts() {
        let t = parse_token_response(r#"{"access_token":"ya29.SECRETVALUE"}"#).unwrap();
        let dbg = format!("{t:?}");
        assert!(!dbg.contains("SECRETVALUE"), "token leaked in Debug: {dbg}");
    }

    #[test]
    fn redact_url_scrubs_token_fragments() {
        let scrubbed = redact_url("error connecting to https://x/token?refresh_token=abc123&z=1");
        assert!(!scrubbed.contains("abc123"), "leaked: {scrubbed}");
        assert!(scrubbed.contains("refresh_token=<redacted>"));
    }
}
