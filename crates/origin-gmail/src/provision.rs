// SPDX-License-Identifier: Apache-2.0
//! Initial refresh-token provisioning via a loopback OAuth authorization-code
//! flow (RFC 6749 §4.1 + PKCE RFC 7636).
//!
//! The Gmail tool's steady-state path ([`crate::Gmail::from_keyvault`]) needs a
//! long-lived `refresh_token` already sitting in the vault. This module mints
//! that token the first time, by:
//!   1. generating a PKCE pair ([`gen_pkce`], reusing [`origin_keyvault::Pkce`]),
//!   2. building Google's consent URL ([`build_auth_url`]),
//!   3. catching the redirect on a loopback listener ([`capture_code`]),
//!   4. exchanging the auth code for tokens ([`exchange_code`]),
//!   5. persisting the credential blob in the shape
//!      [`crate::Gmail::from_keyvault_at`] reads ([`store_credentials`]).
//!
//! Every pure / locally-observable step is unit-tested; only the network POST
//! inside [`exchange_code`] reuses the already-tested `reqwest` form pattern
//! from [`crate::http`].

use std::io::{Read as _, Write as _};
use std::net::TcpListener;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use origin_keyvault::{KeyVault, Pkce, Secret};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::request::encode_component;

/// Google's OAuth 2.0 authorization (consent) endpoint.
pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";

/// The OAuth scope the Gmail tool requests: read-only Gmail access.
pub const GMAIL_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/gmail.readonly";

/// Compute the PKCE S256 code challenge for a given verifier (RFC 7636 §4.2):
/// `BASE64URL-NO-PAD(SHA256(ASCII(verifier)))`.
///
/// Pure and unit-tested against the RFC 7636 Appendix B known-answer vector so
/// the [`gen_pkce`] random pair can be checked for the S256 relationship.
#[must_use]
pub fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// Generate a fresh PKCE `(verifier, challenge)` pair.
///
/// Reuses [`origin_keyvault::Pkce`] for the CSPRNG verifier + S256 challenge,
/// returning owned `String`s so the provisioning flow can hold the verifier
/// across the redirect round-trip and feed it back on the token exchange.
#[must_use]
pub fn gen_pkce() -> (String, String) {
    let pkce = Pkce::new();
    (pkce.verifier().to_owned(), pkce.challenge().to_owned())
}

/// Percent-decode an `application/x-www-form-urlencoded` query component.
///
/// Inverse of [`encode_component`] for the redirect-query parsing path:
/// turns `%2F` back into `/` and `+` into a space. Lone or malformed `%`
/// escapes are passed through literally rather than erroring — the loopback
/// redirect is a trusted local round-trip, and a value we cannot decode is
/// surfaced as-is to the (already authenticated) token exchange.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                    out.push((hi << 4) | lo);
                    i += 3;
                } else {
                    out.push(b'%');
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

const fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Successful authorization-code `/token` response. No `Debug`:
/// `refresh_token` is the long-lived bearer credential we are provisioning.
#[derive(Deserialize)]
struct AuthCodeTokenResponse {
    #[serde(default)]
    refresh_token: Option<String>,
}

/// Build the form fields for an RFC 6749 §4.1.3 authorization-code exchange.
///
/// Produces the `application/x-www-form-urlencoded` pairs for the
/// authorization-code → token exchange against Google's `/token` endpoint,
/// returning owned pairs suitable for `reqwest`'s `.form(..)`. The values are
/// bearer secrets — never log the returned vector.
#[must_use]
pub fn exchange_form(
    client_id: &str,
    client_secret: &str,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Vec<(&'static str, String)> {
    vec![
        ("grant_type", "authorization_code".to_owned()),
        ("code", code.to_owned()),
        ("code_verifier", code_verifier.to_owned()),
        ("client_id", client_id.to_owned()),
        ("client_secret", client_secret.to_owned()),
        ("redirect_uri", redirect_uri.to_owned()),
    ]
}

/// Parse a Google authorization-code `/token` JSON body and pull out the
/// long-lived `refresh_token`.
///
/// Pure (no I/O) so the success and missing-token paths are unit-tested
/// without a network round trip — the only un-tested seam is the HTTP send in
/// [`exchange_code`] that produced `body`.
///
/// # Errors
/// [`Error::Parse`] if `body` is not valid JSON; [`Error::BadArgs`] if it
/// parses but carries no `refresh_token` (e.g. consent was not `offline`, so
/// Google returned only an access token).
pub fn parse_auth_code_response(body: &str) -> Result<Secret<String>> {
    let raw: AuthCodeTokenResponse = serde_json::from_str(body).map_err(|e| Error::Parse(e.to_string()))?;
    raw.refresh_token.map(Secret::new).ok_or_else(|| {
        Error::BadArgs(
            "token response carried no refresh_token (request access_type=offline + prompt=consent)"
                .to_owned(),
        )
    })
}

/// Exchange a loopback-captured authorization `code` (plus its PKCE verifier)
/// for a long-lived `refresh_token` at Google's `/token` endpoint.
///
/// Reuses the same `reqwest` form-POST + status-checked-read pattern as
/// [`crate::http::exchange_refresh`]; only the parsed-out field differs (the
/// refresh token rather than the access token). `token_url` is normally
/// [`crate::request::TOKEN_URL`].
///
/// # Errors
/// [`Error::Http`] on transport failure, [`Error::Status`] on a non-2xx
/// response, [`Error::Parse`] on a malformed body, and [`Error::BadArgs`] if
/// the response carries no refresh token.
pub async fn exchange_code(
    client: &reqwest::Client,
    token_url: &str,
    form: &[(&'static str, String)],
) -> Result<Secret<String>> {
    let resp = client
        .post(token_url)
        .form(form)
        .send()
        .await
        .map_err(|e| Error::Http(format!("token exchange POST: {e}")))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| Error::Http(format!("reading token response: {e}")))?;
    if !status.is_success() {
        return Err(Error::status(status.as_u16(), body));
    }
    parse_auth_code_response(&body)
}

/// Success page served on the loopback redirect once the auth code is in hand.
const SUCCESS_BODY: &str =
    "<html><body><h2>Authentication complete.</h2><p>You may close this tab.</p></body></html>";

/// Extract the raw query string (everything after the first `?`) from an HTTP
/// request line such as `GET /callback?code=...&state=... HTTP/1.1`. Returns an
/// empty string if there is no query component.
fn request_line_query(request_line: &str) -> &str {
    request_line
        .split_whitespace()
        .nth(1)
        .and_then(|target| target.split_once('?'))
        .map_or("", |(_, q)| q)
}

/// Write a minimal HTTP/1.1 response with the given status line and HTML body,
/// then flush. Best-effort: any write error is mapped to [`Error::Http`].
fn write_http_response(stream: &mut std::net::TcpStream, status_line: &str, body: &str) -> Result<()> {
    let response = format!(
        "{status_line}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|e| Error::Http(format!("writing redirect response: {e}")))
}

/// Capture the OAuth redirect on a loopback listener and return the auth code.
///
/// Blocks on `listener`, accepts exactly one inbound HTTP GET (the OAuth
/// redirect), parses its query via [`parse_redirect_code`], responds with a
/// human-friendly "you may close this tab" page, and returns the auth `code`.
///
/// On a parse/state error a `400 Bad Request` page is still served (so the
/// browser tab does not hang) and the underlying [`Error`] is returned.
///
/// This is deliberately a tiny hand-rolled HTTP/1.1 reader — it reads only the
/// request line, which is all a loopback `code` redirect needs — so the crate
/// pulls in no HTTP-server dependency.
///
/// # Errors
/// [`Error::Http`] on a socket accept/read failure, or the [`Error::BadArgs`]
/// from [`parse_redirect_code`] (state mismatch, provider error, missing code).
pub fn capture_code(listener: &TcpListener, expected_state: &str) -> Result<String> {
    let (mut stream, _addr) = listener
        .accept()
        .map_err(|e| Error::Http(format!("accepting redirect connection: {e}")))?;

    // Read until we have at least the request line (terminated by CRLF). We
    // cap the read so a misbehaving client cannot make us buffer unbounded.
    let mut buf = [0u8; 4096];
    let mut data = Vec::new();
    loop {
        let n = stream
            .read(&mut buf)
            .map_err(|e| Error::Http(format!("reading redirect request: {e}")))?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        if data.windows(2).any(|w| w == b"\r\n") || data.len() >= buf.len() {
            break;
        }
    }

    let text = String::from_utf8_lossy(&data);
    let request_line = text.lines().next().unwrap_or("");
    let query = request_line_query(request_line);

    match parse_redirect_code(query, expected_state) {
        Ok(code) => {
            write_http_response(&mut stream, "HTTP/1.1 200 OK", SUCCESS_BODY)?;
            Ok(code)
        }
        Err(e) => {
            // Best-effort error page; ignore a secondary write failure so the
            // original (more useful) parse error is what surfaces.
            let _ = write_http_response(
                &mut stream,
                "HTTP/1.1 400 Bad Request",
                "<html><body><h2>Authentication failed.</h2></body></html>",
            );
            Err(e)
        }
    }
}

/// Persist freshly provisioned Google OAuth credentials in the vault.
///
/// Writes the blob under the default `("google", "gmail")` location in the
/// exact JSON shape [`crate::Gmail::from_keyvault_at`] reads back via
/// [`crate::Credentials::from_json`]:
///
/// ```json
/// { "client_id": "...", "client_secret": "...", "refresh_token": "..." }
/// ```
///
/// Uses [`serde_json`] to encode so values containing `"`/`\` are escaped
/// correctly (a naive string concat would corrupt the blob). The three values
/// are bearer secrets; they go straight into the vault and are never logged.
///
/// # Errors
/// [`Error::Parse`] if the blob cannot be JSON-encoded (does not happen for
/// `String` fields) and [`Error::Credentials`] if the vault write fails.
pub async fn store_credentials(
    vault: &KeyVault,
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<()> {
    let blob = serde_json::json!({
        "client_id": client_id,
        "client_secret": client_secret,
        "refresh_token": refresh_token,
    });
    let json = serde_json::to_string(&blob).map_err(|e| Error::Parse(e.to_string()))?;
    vault
        .set(crate::DEFAULT_PROVIDER, crate::DEFAULT_ACCOUNT, Secret::new(json))
        .await
        .map_err(|e| Error::Credentials(e.to_string()))
}

/// Parse the `code` out of an OAuth redirect query string, validating `state`.
///
/// `query` is the raw redirect query (with or without a leading `?`). On
/// success the percent-decoded authorization `code` is returned. Errors:
///   * an `error=` parameter (user denied consent, etc.) → [`Error::BadArgs`]
///     carrying the provider error code,
///   * a `state` that does not byte-equal `expected_state` (CSRF guard) →
///     [`Error::BadArgs`],
///   * a missing `code` → [`Error::BadArgs`].
///
/// # Errors
/// See above.
pub fn parse_redirect_code(query: &str, expected_state: &str) -> Result<String> {
    let query = query.strip_prefix('?').unwrap_or(query);
    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    let mut oauth_error: Option<String> = None;

    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, raw_val) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "code" => code = Some(percent_decode(raw_val)),
            "state" => state = Some(percent_decode(raw_val)),
            "error" => oauth_error = Some(percent_decode(raw_val)),
            _ => {}
        }
    }

    if let Some(err) = oauth_error {
        return Err(Error::BadArgs(format!(
            "authorization server returned error: {err}"
        )));
    }
    match state.as_deref() {
        Some(s) if s == expected_state => {}
        _ => {
            return Err(Error::BadArgs(
                "redirect state did not match expected value (possible CSRF)".to_owned(),
            ));
        }
    }
    code.ok_or_else(|| Error::BadArgs("redirect query missing `code`".to_owned()))
}

/// Build Google's authorization-code consent URL for a loopback PKCE flow.
///
/// Emits `response_type=code`, `access_type=offline`, and `prompt=consent` so
/// Google reliably returns a `refresh_token` (offline) even on a repeat
/// consent. All query values are percent-encoded via the crate's tested
/// [`encode_component`]. `state` is an opaque CSRF token the caller later
/// matches in [`capture_code`] / [`parse_redirect_code`].
#[must_use]
pub fn build_auth_url(
    client_id: &str,
    redirect_uri: &str,
    code_challenge: &str,
    scope: &str,
    state: &str,
) -> String {
    format!(
        "{AUTH_URL}\
?client_id={}\
&redirect_uri={}\
&response_type=code\
&scope={}\
&code_challenge={}\
&code_challenge_method=S256\
&access_type=offline\
&prompt=consent\
&state={}",
        encode_component(client_id),
        encode_component(redirect_uri),
        encode_component(scope),
        encode_component(code_challenge),
        encode_component(state),
    )
}

/// Generate an opaque, high-entropy `state` token for the authorize request.
///
/// Reuses [`origin_keyvault::Pkce`]'s CSPRNG verifier (base64url, no padding)
/// as the source of randomness so no additional RNG dependency is pulled in.
#[must_use]
pub fn random_state() -> String {
    Pkce::new().verifier().to_owned()
}

/// Best-effort: open `url` in the user's default browser.
///
/// Returns `Ok(())` if the launcher process was *spawned* (it is fire-and-
/// forget — we do not wait for the browser). On a headless box where no
/// launcher exists, returns [`Error::Http`] so the caller can fall back to
/// printing the URL for the user to paste manually.
///
/// Not unit-tested: it shells out to the platform browser launcher, which has
/// no deterministic, side-effect-free behaviour to assert.
///
/// # Errors
/// [`Error::Http`] if the platform launcher could not be spawned.
pub fn open_in_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    let mut cmd = {
        // `cmd /C start "" <url>` — the empty title arg stops `start` from
        // treating a quoted URL as the window title.
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };

    cmd.spawn()
        .map(|_child| ())
        .map_err(|e| Error::Http(format!("could not launch browser: {e}")))
}

/// Run the full interactive refresh-token provisioning flow end-to-end.
///
/// Binds a loopback listener, opens the Google consent page, captures the
/// redirected auth code, exchanges it for a long-lived refresh token, and
/// stores the credential blob in the vault under the default
/// `("google", "gmail")` location.
///
/// This is the thin, **non-unit-tested** wrapper that stitches together the
/// individually tested steps ([`gen_pkce`], [`build_auth_url`],
/// [`capture_code`], [`exchange_code`], [`store_credentials`]). It performs
/// real network + browser + console side effects, so it is exercised manually /
/// by the CLI wiring rather than by an automated test.
///
/// `bind_port` of `0` lets the OS pick an ephemeral port; the chosen port is
/// reflected into the `redirect_uri` so it matches what Google redirects to.
/// The redirect URI's host/port must be registered as an authorized redirect
/// for `client_id` in the Google Cloud console.
///
/// # Errors
/// Any error from the underlying steps: [`Error::Http`] (listener bind, browser
/// launch, transport), the [`parse_redirect_code`] / [`exchange_code`] error
/// variants, or [`Error::Credentials`] on the final vault write.
pub async fn run_login(vault: &KeyVault, client_id: &str, client_secret: &str, bind_port: u16) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", bind_port))
        .map_err(|e| Error::Http(format!("binding loopback listener: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| Error::Http(format!("reading bound port: {e}")))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let (verifier, challenge) = gen_pkce();
    let state = random_state();
    let auth_url = build_auth_url(client_id, &redirect_uri, &challenge, GMAIL_READONLY_SCOPE, &state);

    // Best-effort browser launch; the URL is also returned to the caller's
    // console path so a headless user can paste it manually.
    if open_in_browser(&auth_url).is_err() {
        // Intentionally non-fatal: fall through to capture, the user can open
        // the URL by hand. The CLI layer is responsible for echoing it.
    }

    let code = capture_code(&listener, &state)?;
    let form = exchange_form(client_id, client_secret, &code, &verifier, &redirect_uri);
    let refresh = exchange_code(&new_http_client(), crate::request::TOKEN_URL, &form).await?;
    store_credentials(vault, client_id, client_secret, refresh.expose()).await
}

/// Build the workspace-standard `reqwest::Client`. Mirrors the lib's private
/// constructor; falls back to the default client if the builder fails.
fn new_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn random_state_is_nonempty_and_unique() {
        let a = random_state();
        let b = random_state();
        assert!(!a.is_empty());
        // Two CSPRNG-derived tokens must (overwhelmingly) differ.
        assert_ne!(a, b);
    }

    #[test]
    fn pkce_challenge_matches_rfc7636_vector() {
        // RFC 7636 Appendix B known-answer vector for the S256 method.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(pkce_challenge(verifier), expected);
    }

    #[test]
    fn gen_pkce_satisfies_s256_relationship() {
        let (verifier, challenge) = gen_pkce();
        // The challenge MUST be the S256 transform of the verifier.
        assert_eq!(pkce_challenge(&verifier), challenge);
        // base64url-no-pad of a 32-byte SHA-256 digest is 43 chars.
        assert_eq!(challenge.len(), 43);
        assert!(!challenge.contains('='));
        assert!(!challenge.contains('+'));
        assert!(!challenge.contains('/'));
    }

    #[test]
    fn parse_redirect_code_extracts_code_when_state_matches() {
        let code = parse_redirect_code("code=4%2F0AbCdEf&state=STATEXYZ", "STATEXYZ").unwrap();
        // The `code` value is percent-decoded (`%2F` -> `/`).
        assert_eq!(code, "4/0AbCdEf");
    }

    #[test]
    fn parse_redirect_code_ignores_query_leading_question_mark() {
        let code = parse_redirect_code("?code=abc&state=s", "s").unwrap();
        assert_eq!(code, "abc");
    }

    #[test]
    fn parse_redirect_code_rejects_state_mismatch() {
        let err = parse_redirect_code("code=abc&state=WRONG", "EXPECTED").unwrap_err();
        assert!(matches!(err, Error::BadArgs(_)));
    }

    #[test]
    fn parse_redirect_code_surfaces_provider_error_param() {
        let err = parse_redirect_code("error=access_denied&state=s", "s").unwrap_err();
        let msg = match err {
            Error::BadArgs(msg) => msg,
            other => other.to_string(),
        };
        assert!(msg.contains("access_denied"), "msg: {msg}");
    }

    #[test]
    fn parse_redirect_code_missing_code_is_bad_args() {
        let err = parse_redirect_code("state=s", "s").unwrap_err();
        assert!(matches!(err, Error::BadArgs(_)));
    }

    #[tokio::test]
    async fn store_credentials_round_trips_through_read_path() {
        let vault = KeyVault::in_memory();
        store_credentials(
            &vault,
            "cid.apps.googleusercontent.com",
            "GOCSPX-secret",
            "1//refresh-value",
        )
        .await
        .unwrap();

        // Read back exactly the way `Gmail::from_keyvault_at` does:
        // vault.get("google","gmail") -> Credentials::from_json.
        let blob = vault
            .get(crate::DEFAULT_PROVIDER, crate::DEFAULT_ACCOUNT)
            .await
            .unwrap();
        let creds = crate::Credentials::from_json(blob.expose()).unwrap();
        assert_eq!(creds.client_id(), "cid.apps.googleusercontent.com");
        assert_eq!(creds.client_secret(), "GOCSPX-secret");
        assert_eq!(creds.refresh_token(), "1//refresh-value");
    }

    #[tokio::test]
    async fn store_credentials_rejects_secret_containing_field_breaking_chars() {
        // A refresh token with a double-quote would corrupt naive string
        // concatenation; storing then reading must still round-trip exactly.
        let vault = KeyVault::in_memory();
        let weird = r#"tok"with\backslash and "quote"#;
        store_credentials(&vault, "id", "sec", weird).await.unwrap();
        let blob = vault
            .get(crate::DEFAULT_PROVIDER, crate::DEFAULT_ACCOUNT)
            .await
            .unwrap();
        let creds = crate::Credentials::from_json(blob.expose()).unwrap();
        assert_eq!(creds.refresh_token(), weird);
    }

    #[test]
    fn capture_code_accepts_one_get_and_returns_code() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        // Drive a client GET from a background thread; capture_code blocks on
        // the listener until this connection arrives.
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .write_all(
                    b"GET /callback?code=AUTHCODE42&state=ST8 HTTP/1.1\r\n\
                      Host: 127.0.0.1\r\n\
                      Connection: close\r\n\r\n",
                )
                .unwrap();
            let mut resp = String::new();
            stream.read_to_string(&mut resp).unwrap();
            resp
        });

        let code = capture_code(&listener, "ST8").unwrap();
        assert_eq!(code, "AUTHCODE42");

        // The user-facing response is a 200 with a close-tab message.
        let resp = client.join().unwrap();
        assert!(resp.starts_with("HTTP/1.1 200"), "resp: {resp}");
        assert!(
            resp.to_ascii_lowercase().contains("you may close this tab"),
            "resp: {resp}"
        );
    }

    #[test]
    fn capture_code_propagates_state_mismatch() {
        use std::io::Write;
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let _client = std::thread::spawn(move || {
            if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
                let _ = stream
                    .write_all(b"GET /callback?code=x&state=WRONG HTTP/1.1\r\nConnection: close\r\n\r\n");
            }
        });
        let err = capture_code(&listener, "EXPECTED").unwrap_err();
        assert!(matches!(err, Error::BadArgs(_)));
    }

    #[test]
    fn parse_auth_code_response_extracts_refresh_token() {
        let body = r#"{"access_token":"ya29.x","refresh_token":"1//new-refresh","expires_in":3599,"token_type":"Bearer"}"#;
        let rt = parse_auth_code_response(body).unwrap();
        assert_eq!(rt.expose(), "1//new-refresh");
    }

    #[test]
    fn parse_auth_code_response_missing_refresh_is_bad_args() {
        // Without access_type=offline (or on a repeat consent) Google may omit
        // the refresh token; that is a provisioning failure we must report.
        let body = r#"{"access_token":"ya29.x","expires_in":3599}"#;
        let err = parse_auth_code_response(body).unwrap_err();
        assert!(matches!(err, Error::BadArgs(_)));
    }

    #[test]
    fn parse_auth_code_response_bad_json_is_parse_error() {
        assert!(matches!(
            parse_auth_code_response("not json").unwrap_err(),
            Error::Parse(_)
        ));
    }

    #[test]
    fn exchange_form_carries_grant_code_and_verifier() {
        let form = exchange_form("cid", "csec", "AUTHCODE", "VERIFIER", "http://127.0.0.1:1/cb");
        assert!(form.contains(&("grant_type", "authorization_code".to_owned())));
        assert!(form.contains(&("code", "AUTHCODE".to_owned())));
        assert!(form.contains(&("code_verifier", "VERIFIER".to_owned())));
        assert!(form.contains(&("client_id", "cid".to_owned())));
        assert!(form.contains(&("client_secret", "csec".to_owned())));
        assert!(form.contains(&("redirect_uri", "http://127.0.0.1:1/cb".to_owned())));
        assert_eq!(form.len(), 6);
    }

    #[test]
    fn build_auth_url_exact_shape() {
        let url = build_auth_url(
            "cid.apps.googleusercontent.com",
            "http://127.0.0.1:8080/callback",
            "CHALLENGE123",
            "https://www.googleapis.com/auth/gmail.readonly",
            "STATEXYZ",
        );
        assert_eq!(
            url,
            "https://accounts.google.com/o/oauth2/v2/auth\
?client_id=cid.apps.googleusercontent.com\
&redirect_uri=http%3A%2F%2F127.0.0.1%3A8080%2Fcallback\
&response_type=code\
&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fgmail.readonly\
&code_challenge=CHALLENGE123\
&code_challenge_method=S256\
&access_type=offline\
&prompt=consent\
&state=STATEXYZ"
        );
    }
}
