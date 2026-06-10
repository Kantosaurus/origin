// SPDX-License-Identifier: Apache-2.0
//! Deterministic, secret-safe HTTP cassette recording and sequential replay.
//!
//! `origin`'s test and offline-replay story needs reproducible HTTP fixtures the
//! way opencode's http-recorder and VCR-style libraries record interactions once
//! and replay them deterministically. This crate stores request/response shapes,
//! matches them sequentially during replay, and — uniquely — refuses to persist a
//! cassette that still carries a live credential.
//!
//! Secret safety is the differentiator: [`scrub_secrets`] redacts auth headers and
//! bearer/`sk-` tokens in place, and [`assert_redacted`] is a CI gate that blocks a
//! save the moment any header or body still smells like a secret. The scan in
//! [`contains_secret`] is regex-free (no `regex`/`once_cell` dependency, MSRV-safe)
//! and runs on pure byte/char heuristics.
//!
//! ```
//! use origin_cassette::{Cassette, Interaction, ReqShape, RespShape, scrub_secrets};
//!
//! let mut cassette = Cassette::new("login");
//! cassette.record(Interaction {
//!     request: ReqShape {
//!         method: "POST".into(),
//!         url: "https://api.example.com/v1/chat".into(),
//!         headers: vec![("authorization".into(), "Bearer sk-live-abc123".into())],
//!         body: String::new(),
//!     },
//!     response: RespShape { status: 200, headers: vec![], body: "{\"ok\":true}".into() },
//! });
//! let redacted = scrub_secrets(&mut cassette);
//! assert_eq!(redacted, 1);
//! ```

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Sentinel written in place of any redacted secret value.
pub const REDACTED: &str = "***";

/// Errors produced when (de)serializing or guarding a cassette.
#[derive(Debug, thiserror::Error)]
pub enum CassetteError {
    /// Serialization or deserialization failed.
    #[error("cassette serde error: {0}")]
    Serde(String),
    /// A save was refused because a header or body still contained a secret.
    /// The payload describes where the leak was found.
    #[error("refusing to persist cassette: unredacted secret in {0}")]
    UnredactedSecret(String),
}

/// The shape of an HTTP request as stored in a cassette.
///
/// Only the fields that affect matching and replay are kept; this keeps the
/// on-disk form stable and free of transport noise (connection timing, sockets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReqShape {
    /// HTTP method, e.g. `GET` or `POST`.
    pub method: String,
    /// Fully-qualified request URL.
    pub url: String,
    /// Request headers as ordered `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// Request body, captured as UTF-8 text.
    pub body: String,
}

/// The shape of an HTTP response as stored in a cassette.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RespShape {
    /// HTTP status code.
    pub status: u16,
    /// Response headers as ordered `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// Response body, captured as UTF-8 text.
    pub body: String,
}

/// A single recorded request/response pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interaction {
    /// The captured request.
    pub request: ReqShape,
    /// The captured response.
    pub response: RespShape,
}

/// A named, ordered collection of recorded HTTP interactions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cassette {
    /// Human-readable cassette name (typically the test or scenario name).
    pub name: String,
    /// Interactions in record order; replay matching consumes them sequentially.
    pub interactions: Vec<Interaction>,
}

impl Cassette {
    /// Create an empty cassette with the given name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            interactions: Vec::new(),
        }
    }

    /// Append an interaction to the cassette in record order.
    pub fn record(&mut self, interaction: Interaction) {
        self.interactions.push(interaction);
    }

    /// Find the next interaction whose request matches `req` by `(method, url)`
    /// shape, scanning forward from the start.
    ///
    /// Matching is intentionally shape-based (method + url) rather than byte-exact:
    /// headers and bodies vary across runs (timestamps, nonces) but the call
    /// sequence is stable. The match is case-insensitive on the method.
    ///
    /// Returns `None` when no recorded request shares the shape.
    #[must_use]
    pub fn match_next(&self, req: &ReqShape) -> Option<&Interaction> {
        self.interactions.iter().find(|i| shapes_match(&i.request, req))
    }

    /// Serialize the cassette to pretty JSON.
    ///
    /// # Errors
    ///
    /// Returns [`CassetteError::Serde`] if serialization fails.
    pub fn to_json(&self) -> Result<String, CassetteError> {
        serde_json::to_string_pretty(self).map_err(|e| CassetteError::Serde(e.to_string()))
    }

    /// Deserialize a cassette from JSON.
    ///
    /// # Errors
    ///
    /// Returns [`CassetteError::Serde`] if the input is not a valid cassette.
    pub fn from_json(s: &str) -> Result<Self, CassetteError> {
        serde_json::from_str(s).map_err(|e| CassetteError::Serde(e.to_string()))
    }
}

/// Two request shapes match when their methods (case-insensitive) and URLs agree.
///
/// URLs are compared after redacting embedded secrets (userinfo and secret-like
/// query/path tokens) so a *scrubbed* recorded URL still matches the *live*
/// probe URL on replay — e.g. a provider that carries its key as `?key=...`
/// (Gemini) is persisted redacted yet replays correctly.
fn shapes_match(recorded: &ReqShape, probe: &ReqShape) -> bool {
    redact_url_secrets(&recorded.url) == redact_url_secrets(&probe.url)
        && recorded.method.eq_ignore_ascii_case(&probe.method)
}

/// Header names whose values are always treated as secrets and fully redacted.
const SECRET_HEADERS: &[&str] = &["authorization", "x-api-key", "proxy-authorization"];

/// Redact secrets across every interaction in `c`, in place.
///
/// Redacts the value of any [`SECRET_HEADERS`] header and rewrites
/// bearer/`sk-`/`api_key=` tokens found in request and response bodies to
/// [`REDACTED`]. Returns the number of distinct values redacted (one per header
/// value and one per body that changed), so callers can log how much was scrubbed.
pub fn scrub_secrets(c: &mut Cassette) -> usize {
    let mut redacted = 0usize;
    for interaction in &mut c.interactions {
        if scrub_url(&mut interaction.request.url) {
            redacted += 1;
        }
        redacted += scrub_headers(&mut interaction.request.headers);
        redacted += scrub_headers(&mut interaction.response.headers);
        if scrub_body(&mut interaction.request.body) {
            redacted += 1;
        }
        if scrub_body(&mut interaction.response.body) {
            redacted += 1;
        }
    }
    redacted
}

/// Redact credentials embedded in a request URL in place; returns `true` when
/// the URL changed. Covers the userinfo component (`user:pass@host`) and any
/// secret-looking query/path tokens (`api_key=`, `sk-`, Google `?key=` values,
/// long opaque tokens) via the shared body scrubber.
fn scrub_url(url: &mut String) -> bool {
    let rewritten = redact_url_secrets(url);
    if rewritten == *url {
        false
    } else {
        *url = rewritten;
        true
    }
}

/// Deterministically redact the secret-bearing parts of a URL: replace the
/// authority userinfo with [`REDACTED`] and run the token scrubber over the
/// rest. Pure and idempotent, so it can both sanitize a stored URL and
/// canonicalize URLs for replay matching.
fn redact_url_secrets(url: &str) -> String {
    redact_tokens(&redact_userinfo(url))
}

/// Replace the userinfo of a URL authority (`scheme://user:pass@host/...`) with
/// the [`REDACTED`] sentinel, preserving scheme, host, port, and path. Inputs
/// without a scheme separator or without userinfo are returned unchanged.
fn redact_userinfo(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_owned();
    };
    let authority_start = scheme_end + 3;
    let rest = &url[authority_start..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let Some(at) = authority.rfind('@') else {
        return url.to_owned();
    };
    if &authority[..at] == REDACTED {
        return url.to_owned();
    }
    let host = &authority[at + 1..];
    format!(
        "{}{REDACTED}@{host}{}",
        &url[..authority_start],
        &rest[authority_end..]
    )
}

/// `true` when a URL authority still carries live (non-redacted) userinfo.
fn url_has_live_userinfo(url: &str) -> bool {
    let Some(scheme_end) = url.find("://") else {
        return false;
    };
    let rest = &url[scheme_end + 3..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    authority
        .rfind('@')
        .is_some_and(|at| !authority[..at].is_empty() && &authority[..at] != REDACTED)
}

/// Redact secret-bearing header values; returns how many were changed.
fn scrub_headers(headers: &mut [(String, String)]) -> usize {
    let mut count = 0usize;
    for (name, value) in headers.iter_mut() {
        if is_secret_header(name) && value != REDACTED {
            *value = REDACTED.to_string();
            count += 1;
        }
    }
    count
}

/// Rewrite secret tokens inside a body; returns `true` if anything changed.
fn scrub_body(body: &mut String) -> bool {
    if body.is_empty() {
        return false;
    }
    let rewritten = redact_tokens(body);
    if &rewritten == body {
        false
    } else {
        *body = rewritten;
        true
    }
}

/// Replace each secret-looking token in `s` with [`REDACTED`].
///
/// Tokenizes on whitespace and a small set of delimiters so surrounding JSON
/// punctuation (`"`, `,`, `}`) is preserved while the credential itself is
/// scrubbed. `Bearer <token>` collapses to `Bearer ***`.
fn redact_tokens(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut token = String::new();
    let mut prev_was_bearer = false;
    for ch in s.chars() {
        if is_token_char(ch) {
            token.push(ch);
        } else {
            prev_was_bearer = flush_token(&mut out, &mut token, prev_was_bearer);
            out.push(ch);
            if !ch.is_whitespace() {
                prev_was_bearer = false;
            }
        }
    }
    flush_token(&mut out, &mut token, prev_was_bearer);
    out
}

/// Emit `token` into `out`, redacting it when it (or its `Bearer` lead-in) is a
/// secret. Returns whether the just-emitted token was the literal `Bearer`.
fn flush_token(out: &mut String, token: &mut String, prev_was_bearer: bool) -> bool {
    if token.is_empty() {
        return prev_was_bearer;
    }
    let is_bearer_keyword = token == "Bearer";
    if prev_was_bearer || token_is_secret(token) {
        out.push_str(REDACTED);
    } else {
        out.push_str(token);
    }
    token.clear();
    is_bearer_keyword
}

/// Characters that are part of a credential token (alnum plus token punctuation).
const fn is_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '+' | '/' | '=')
}

/// `true` when `name` is a header whose value must always be redacted.
fn is_secret_header(name: &str) -> bool {
    SECRET_HEADERS.iter().any(|h| name.eq_ignore_ascii_case(h))
}

/// Whether a standalone token looks like a secret (used by body scrubbing).
fn token_is_secret(token: &str) -> bool {
    if let Some(rest) = token.strip_prefix("api_key=") {
        return !rest.is_empty();
    }
    token.starts_with("sk-") || looks_like_opaque_token(token)
}

/// Scan `s` for any sign of an embedded secret without using a regex engine.
///
/// Detects the `sk-` prefix, a `Bearer ` lead-in, an `api_key=` query parameter,
/// and long opaque hex/base64-ish tokens. A marker followed only by the
/// [`REDACTED`] sentinel is treated as already-scrubbed (so `Bearer ***` is
/// clean). Designed to be conservative enough to gate CI saves yet free of false
/// positives on ordinary prose, JSON, and previously-redacted values.
#[must_use]
pub fn contains_secret(s: &str) -> bool {
    if marker_has_live_value(s, "Bearer ") || marker_has_live_value(s, "api_key=") {
        return true;
    }
    // `sk-` is only a secret when it is the actual prefix of a token, not part of
    // a redacted `***` or an ordinary word boundary; check the following char.
    if has_live_sk_prefix(s) {
        return true;
    }
    s.split(|c: char| !is_token_char(c)).any(looks_like_opaque_token)
}

/// `true` when `marker` appears in `s` followed by something other than the
/// [`REDACTED`] sentinel (i.e. a still-live credential).
fn marker_has_live_value(s: &str, marker: &str) -> bool {
    s.match_indices(marker).any(|(idx, _)| {
        let rest = &s[idx + marker.len()..];
        !rest.starts_with(REDACTED) && rest.chars().next().is_some_and(is_token_char)
    })
}

/// `true` when `sk-` begins a non-empty token somewhere in `s`.
fn has_live_sk_prefix(s: &str) -> bool {
    s.match_indices("sk-").any(|(idx, _)| {
        let rest = &s[idx + 3..];
        rest.chars().next().is_some_and(is_token_char)
    })
}

/// Heuristic: a long, high-entropy-looking token of hex/base64 alphabet.
///
/// Requires a minimum length and that the token be composed only of
/// base64/hex-url characters with at least one digit, which filters out ordinary
/// English words while catching API keys and session tokens.
fn looks_like_opaque_token(token: &str) -> bool {
    const MIN_LEN: usize = 32;
    if token.len() < MIN_LEN {
        return false;
    }
    let all_token_alphabet = token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '/' | '='));
    let has_digit = token.chars().any(|c| c.is_ascii_digit());
    let has_alpha = token.chars().any(|c| c.is_ascii_alphabetic());
    all_token_alphabet && has_digit && has_alpha
}

/// Assert that `c` carries no unredacted secret, for use as a CI save gate.
///
/// Scans every request/response header and body. The first leak short-circuits
/// with a description of its location.
///
/// # Errors
///
/// Returns [`CassetteError::UnredactedSecret`] describing the first leak found.
pub fn assert_redacted(c: &Cassette) -> Result<(), CassetteError> {
    for (idx, interaction) in c.interactions.iter().enumerate() {
        check_url(idx, &interaction.request.url)?;
        check_headers(idx, "request", &interaction.request.headers)?;
        check_body(idx, "request", &interaction.request.body)?;
        check_headers(idx, "response", &interaction.response.headers)?;
        check_body(idx, "response", &interaction.response.body)?;
    }
    Ok(())
}

/// Gate a request URL: reject live userinfo or any secret-looking token.
fn check_url(idx: usize, url: &str) -> Result<(), CassetteError> {
    if url_has_live_userinfo(url) || contains_secret(url) {
        return Err(CassetteError::UnredactedSecret(format!(
            "interaction {idx} request url"
        )));
    }
    Ok(())
}

/// Gate a single header list, mapping any leak to a located error.
fn check_headers(idx: usize, side: &str, headers: &[(String, String)]) -> Result<(), CassetteError> {
    for (name, value) in headers {
        if is_secret_header(name) && value != REDACTED {
            return Err(CassetteError::UnredactedSecret(format!(
                "interaction {idx} {side} header `{name}`"
            )));
        }
        if contains_secret(value) {
            return Err(CassetteError::UnredactedSecret(format!(
                "interaction {idx} {side} header `{name}` value"
            )));
        }
    }
    Ok(())
}

/// Gate a single body, mapping any leak to a located error.
fn check_body(idx: usize, side: &str, body: &str) -> Result<(), CassetteError> {
    if contains_secret(body) {
        return Err(CassetteError::UnredactedSecret(format!(
            "interaction {idx} {side} body"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    fn req(method: &str, url: &str) -> ReqShape {
        ReqShape {
            method: method.to_string(),
            url: url.to_string(),
            headers: Vec::new(),
            body: String::new(),
        }
    }

    fn resp(status: u16, body: &str) -> RespShape {
        RespShape {
            status,
            headers: Vec::new(),
            body: body.to_string(),
        }
    }

    #[test]
    fn scrub_redacts_authorization_header_and_sk_body_token() {
        let mut c = Cassette::new("auth");
        let mut request = req("POST", "https://api.example.com/v1/chat");
        request
            .headers
            .push(("Authorization".to_string(), "Bearer sk-live-xyz".to_string()));
        c.record(Interaction {
            request,
            response: resp(200, "{\"key\":\"sk-live-deadbeef\"}"),
        });

        let n = scrub_secrets(&mut c);
        // One header value + one response body.
        assert_eq!(n, 2);
        assert_eq!(c.interactions[0].request.headers[0].1, REDACTED);
        assert!(!contains_secret(&c.interactions[0].response.body));
        assert!(c.interactions[0].response.body.contains(REDACTED));
    }

    #[test]
    fn scrub_redacts_url_embedded_key_and_gate_catches_it() {
        // Gemini-style key in the query string, plus userinfo credentials.
        let secret_url =
            "https://user:p4ssw0rd@generativelanguage.googleapis.com/v1/models?key=AIzaSyD1234567890abcDEFghijKLmnoPQRstuv";
        let mut c = Cassette::new("urlkey");
        c.record(Interaction {
            request: req("POST", secret_url),
            response: resp(200, "ok"),
        });

        // Before scrubbing, the CI gate must reject the unredacted URL.
        assert!(assert_redacted(&c).is_err(), "gate must flag URL-embedded secret");

        let n = scrub_secrets(&mut c);
        assert!(n >= 1, "URL redaction should count");
        let scrubbed = &c.interactions[0].request.url;
        assert!(!scrubbed.contains("AIzaSyD1234567890abcDEFghijKLmnoPQRstuv"), "key leaked: {scrubbed}");
        assert!(!scrubbed.contains("p4ssw0rd"), "userinfo leaked: {scrubbed}");
        assert!(scrubbed.contains("generativelanguage.googleapis.com"), "host must survive: {scrubbed}");

        // After scrubbing, the gate passes.
        assert!(assert_redacted(&c).is_ok(), "scrubbed cassette must pass the gate");

        // Replay still matches: the live probe carries the real key, the stored
        // interaction is redacted, yet shape-matching canonicalizes both.
        let probe = req("POST", secret_url);
        assert!(
            c.match_next(&probe).is_some(),
            "redacted recording must still match the live secret-bearing probe"
        );
    }

    #[test]
    fn scrub_collapses_bearer_token_in_body() {
        let mut c = Cassette::new("bearer");
        c.record(Interaction {
            request: req("GET", "https://h/x"),
            response: resp(200, "header: Bearer abcDEF123ghiJKL456mnoPQR789stuVWX012"),
        });
        let n = scrub_secrets(&mut c);
        assert_eq!(n, 1);
        let body = &c.interactions[0].response.body;
        assert!(body.contains("Bearer ***"), "got: {body}");
        assert!(!contains_secret(body));
    }

    #[test]
    fn match_next_is_sequential_by_shape() {
        let mut c = Cassette::new("seq");
        c.record(Interaction {
            request: req("GET", "https://h/a"),
            response: resp(200, "a"),
        });
        c.record(Interaction {
            request: req("POST", "https://h/b"),
            response: resp(201, "b"),
        });

        // Method match is case-insensitive; body/headers on the probe are ignored.
        let hit = c.match_next(&req("get", "https://h/a")).unwrap();
        assert_eq!(hit.response.body, "a");
        let hit_b = c.match_next(&req("POST", "https://h/b")).unwrap();
        assert_eq!(hit_b.response.status, 201);
        assert!(c.match_next(&req("GET", "https://h/missing")).is_none());
        // Same url but wrong method does not match.
        assert!(c.match_next(&req("DELETE", "https://h/a")).is_none());
    }

    #[test]
    fn assert_redacted_fails_on_raw_secret() {
        let mut c = Cassette::new("leak");
        let mut request = req("POST", "https://h/x");
        request
            .headers
            .push(("authorization".to_string(), "Bearer sk-raw".to_string()));
        c.record(Interaction {
            request,
            response: resp(200, "ok"),
        });
        let err = assert_redacted(&c).unwrap_err();
        assert!(matches!(err, CassetteError::UnredactedSecret(_)));

        // After scrubbing it passes.
        scrub_secrets(&mut c);
        assert!(assert_redacted(&c).is_ok());
    }

    #[test]
    fn assert_redacted_catches_body_leak() {
        let mut c = Cassette::new("bodyleak");
        c.record(Interaction {
            // Clean URL so the gate reaches the body check (URL-embedded secrets
            // are covered separately by scrub_redacts_url_embedded_key_*).
            request: req("GET", "https://h/x"),
            response: resp(200, "ok"),
        });
        c.interactions[0].request.body = "token=sk-abc".to_string();
        let err = assert_redacted(&c).unwrap_err();
        let CassetteError::UnredactedSecret(loc) = err else {
            unreachable!("expected an UnredactedSecret variant")
        };
        assert!(loc.contains("request body"), "got: {loc}");
    }

    #[test]
    fn json_round_trip_is_lossless() {
        let mut c = Cassette::new("rt");
        let mut request = req("POST", "https://h/x");
        request.headers.push(("x-test".into(), "1".into()));
        request.body = "{\"a\":1}".into();
        c.record(Interaction {
            request,
            response: resp(200, "{\"ok\":true}"),
        });

        let json = c.to_json().unwrap();
        let back = Cassette::from_json(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn from_json_rejects_garbage() {
        let err = Cassette::from_json("not json").unwrap_err();
        assert!(matches!(err, CassetteError::Serde(_)));
    }

    #[test]
    fn contains_secret_heuristics() {
        assert!(contains_secret("authorization: Bearer foo"));
        assert!(contains_secret("key=sk-12345"));
        assert!(contains_secret("https://h/x?api_key=abc"));
        // Long opaque token (>= 32 chars, mixed alnum).
        assert!(contains_secret("token abcDEF123ghiJKL456mnoPQR789stuVWX012"));
        // Ordinary prose and short values are not flagged.
        assert!(!contains_secret("the quick brown fox jumps over the lazy dog"));
        assert!(!contains_secret("status: 200 ok"));
        assert!(!contains_secret(""));
    }

    #[test]
    fn scrub_is_idempotent() {
        let mut c = Cassette::new("idem");
        let mut request = req("POST", "https://h/x");
        request
            .headers
            .push(("authorization".into(), "Bearer sk-x".into()));
        c.record(Interaction {
            request,
            response: resp(200, "{\"k\":\"sk-secretdeadbeef\"}"),
        });
        let first = scrub_secrets(&mut c);
        assert!(first >= 2);
        // Second pass finds nothing new to redact.
        let second = scrub_secrets(&mut c);
        assert_eq!(second, 0);
        assert!(assert_redacted(&c).is_ok());
    }
}
