// SPDX-License-Identifier: Apache-2.0
//! Workload Identity Federation / OIDC token-exchange helpers for keyless CI auth.
//!
//! `origin` baseline ships only static-API-key auth; CI runners then leak a
//! long-lived secret. This crate adds keyless Workload Identity Federation
//! (claude-code WIF: `anthropic_federation_rule_id` / `ANTHROPIC_WORKSPACE_ID`)
//! by building the RFC 8693 token-exchange request and parsing the response — a
//! pure request-builder + response-parser. The HTTP POST stays the caller's job,
//! so the crate has no network, no async, and no extra base64 dependency (the
//! JWT payload is decoded with a hand-rolled base64url table).
//!
//! ```
//! use origin_oidc::{ExchangeRequest, build_exchange_form, parse_token_response, is_expired};
//!
//! let req = ExchangeRequest {
//!     token_url: "https://sts.example/token".into(),
//!     subject_token: "ci.oidc.jwt".into(),
//!     audience: "anthropic".into(),
//!     workspace_id: Some("ws_123".into()),
//!     federation_rule_id: Some("rule_456".into()),
//! };
//! let form = build_exchange_form(&req);
//! assert!(form.iter().any(|(k, _)| k == "grant_type"));
//!
//! // caller POSTs `form` to req.token_url, then:
//! let tok = parse_token_response(r#"{"access_token":"a","expires_in":3600,"token_type":"Bearer"}"#).unwrap();
//! assert!(!is_expired(&tok, 0, 60));
//! ```

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// RFC 8693 token-exchange grant type.
pub const GRANT_TYPE_TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";

/// RFC 8693 subject-token type for a JWT (the CI-provided OIDC id token).
pub const SUBJECT_TOKEN_TYPE_JWT: &str = "urn:ietf:params:oauth:token-type:jwt";

/// Inputs for a Workload Identity Federation token exchange.
///
/// The caller fills these from the CI environment (e.g. the runner's OIDC id
/// token as [`subject_token`](Self::subject_token), plus
/// `ANTHROPIC_WORKSPACE_ID` / `anthropic_federation_rule_id`) before `POST`ing the
/// form built by [`build_exchange_form`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeRequest {
    /// STS endpoint that performs the exchange (the caller `POST`s the form here).
    pub token_url: String,
    /// CI-provided OIDC id token (a JWT) presented as the subject.
    pub subject_token: String,
    /// Target audience the exchanged token is requested for.
    pub audience: String,
    /// Optional Anthropic workspace id (`ANTHROPIC_WORKSPACE_ID`).
    pub workspace_id: Option<String>,
    /// Optional federation rule id (`anthropic_federation_rule_id`).
    pub federation_rule_id: Option<String>,
}

/// Build the `application/x-www-form-urlencoded` field pairs for an RFC 8693
/// token exchange.
///
/// The returned pairs are unencoded `(name, value)` tuples in a stable order:
/// `grant_type`, `subject_token`, `subject_token_type`, `audience`, then the
/// optional `workspace_id` and `federation_rule_id` when present. The caller is
/// responsible for URL-encoding and `POST`ing them to
/// [`ExchangeRequest::token_url`].
#[must_use]
pub fn build_exchange_form(req: &ExchangeRequest) -> Vec<(String, String)> {
    let mut form = Vec::with_capacity(6);
    form.push((
        "grant_type".to_string(),
        GRANT_TYPE_TOKEN_EXCHANGE.to_string(),
    ));
    form.push(("subject_token".to_string(), req.subject_token.clone()));
    form.push((
        "subject_token_type".to_string(),
        SUBJECT_TOKEN_TYPE_JWT.to_string(),
    ));
    form.push(("audience".to_string(), req.audience.clone()));
    if let Some(ws) = &req.workspace_id {
        form.push(("workspace_id".to_string(), ws.clone()));
    }
    if let Some(rule) = &req.federation_rule_id {
        form.push(("federation_rule_id".to_string(), rule.clone()));
    }
    form
}

/// A successfully exchanged short-lived access token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangedToken {
    /// The bearer access token to send on subsequent API requests.
    pub access_token: String,
    /// Lifetime in seconds from issuance.
    pub expires_in: u64,
    /// Token type, conventionally `Bearer`.
    pub token_type: String,
}

/// Wire shape of the STS JSON response. `expires_in` may legitimately arrive as
/// a JSON number or a quoted string, so it is captured loosely and normalized.
#[derive(Debug, Deserialize)]
struct RawTokenResponse {
    access_token: Option<String>,
    expires_in: Option<serde_json::Value>,
    token_type: Option<String>,
}

/// Parse an STS token-exchange JSON response into an [`ExchangedToken`].
///
/// # Errors
///
/// Returns [`OidcError::Parse`] when the body is not valid JSON, when the
/// required `access_token` is missing, or when `expires_in` is present but is
/// neither a non-negative number nor a numeric string.
pub fn parse_token_response(json: &str) -> Result<ExchangedToken, OidcError> {
    let raw: RawTokenResponse =
        serde_json::from_str(json).map_err(|e| OidcError::Parse(e.to_string()))?;

    let access_token = raw
        .access_token
        .filter(|t| !t.is_empty())
        .ok_or_else(|| OidcError::Parse("missing access_token".to_string()))?;

    let expires_in = match raw.expires_in {
        None => 0,
        Some(v) => expires_in_from_value(&v)?,
    };

    let token_type = raw.token_type.unwrap_or_else(|| "Bearer".to_string());

    Ok(ExchangedToken {
        access_token,
        expires_in,
        token_type,
    })
}

/// Coerce a JSON `expires_in` (number or numeric string) into seconds.
fn expires_in_from_value(v: &serde_json::Value) -> Result<u64, OidcError> {
    if let Some(n) = v.as_u64() {
        return Ok(n);
    }
    if let Some(f) = v.as_f64() {
        if f.is_finite() && f >= 0.0 {
            // `f` is finite and non-negative here; truncation toward zero is intended.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            return Ok(f as u64);
        }
        return Err(OidcError::Parse("expires_in out of range".to_string()));
    }
    if let Some(s) = v.as_str() {
        return s
            .trim()
            .parse::<u64>()
            .map_err(|_| OidcError::Parse(format!("expires_in not numeric: {s}")));
    }
    Err(OidcError::Parse("expires_in has unexpected type".to_string()))
}

/// Decoded JWT claims relevant to federation (no signature verification).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// Issuer (`iss`).
    pub iss: String,
    /// Subject (`sub`).
    pub sub: String,
    /// Audience (`aud`).
    pub aud: String,
    /// Expiry as a Unix timestamp in seconds (`exp`).
    pub exp: u64,
}

/// Loose view of the JWT payload: `aud` may be a string or an array of strings.
#[derive(Debug, Deserialize)]
struct RawClaims {
    iss: Option<String>,
    sub: Option<String>,
    aud: Option<serde_json::Value>,
    exp: Option<serde_json::Value>,
}

/// Decode the claims from a JWT's payload segment.
///
/// This base64url-decodes the middle (payload) segment of a `header.payload.signature`
/// JWT and reads `iss`, `sub`, `aud`, and `exp`. It performs **no** signature
/// verification — validating the signature is the identity provider's job; this
/// is for inspecting/logging the federated identity only.
///
/// # Errors
///
/// Returns [`OidcError::Jwt`] when the token does not have three
/// dot-separated segments, when the payload is not valid base64url, or when the
/// decoded payload is not valid UTF-8 JSON with the expected claim shapes.
pub fn parse_oidc_claims(jwt: &str) -> Result<Claims, OidcError> {
    let mut parts = jwt.split('.');
    let (Some(_header), Some(payload), Some(_sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(OidcError::Jwt(
            "expected 3 dot-separated JWT segments".to_string(),
        ));
    };

    let bytes = base64url_decode(payload)?;
    let text = String::from_utf8(bytes)
        .map_err(|e| OidcError::Jwt(format!("payload not UTF-8: {e}")))?;
    let raw: RawClaims =
        serde_json::from_str(&text).map_err(|e| OidcError::Jwt(format!("payload not JSON: {e}")))?;

    let iss = raw.iss.unwrap_or_default();
    let sub = raw.sub.unwrap_or_default();
    let aud = aud_to_string(raw.aud.as_ref());
    let exp = raw
        .exp
        .as_ref()
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);

    Ok(Claims {
        iss,
        sub,
        aud,
        exp,
    })
}

/// Normalize an `aud` claim (string, or first element of an array) to a string.
fn aud_to_string(aud: Option<&serde_json::Value>) -> String {
    match aud {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .find_map(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// Report whether `tok` is expired at `now_unix`, treating the token as expired
/// `skew_secs` early to avoid races at the boundary.
///
/// Compares against the token's issuance-relative lifetime is **not** possible
/// from [`ExchangedToken`] alone (it carries only `expires_in`), so this treats
/// `expires_in` as the absolute deadline measured against `now_unix`; callers
/// that issued the token at a known instant should pass `now_unix` relative to
/// that instant. The token is considered expired when
/// `now_unix + skew_secs >= expires_in`.
#[must_use]
pub const fn is_expired(tok: &ExchangedToken, now_unix: u64, skew_secs: u64) -> bool {
    now_unix.saturating_add(skew_secs) >= tok.expires_in
}

/// Decode a base64url (RFC 4648 §5, no padding required) string into bytes.
fn base64url_decode(input: &str) -> Result<Vec<u8>, OidcError> {
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let mut out: Vec<u8> = Vec::with_capacity(input.len() * 3 / 4 + 3);

    for ch in input.bytes() {
        if ch == b'=' {
            break;
        }
        let val = base64url_value(ch)
            .ok_or_else(|| OidcError::Jwt(format!("invalid base64url byte: {ch:#04x}")))?;
        acc = (acc << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            // Take the top 8 bits; mask keeps it within a byte.
            #[allow(clippy::cast_possible_truncation)]
            out.push(((acc >> bits) & 0xFF) as u8);
        }
    }
    Ok(out)
}

/// Map a single base64url character to its 6-bit value.
const fn base64url_value(ch: u8) -> Option<u8> {
    match ch {
        b'A'..=b'Z' => Some(ch - b'A'),
        b'a'..=b'z' => Some(ch - b'a' + 26),
        b'0'..=b'9' => Some(ch - b'0' + 52),
        b'-' => Some(62),
        b'_' => Some(63),
        _ => None,
    }
}

/// Errors produced while building or parsing OIDC token-exchange data.
#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    /// The STS response body could not be parsed into an [`ExchangedToken`].
    #[error("token response parse error: {0}")]
    Parse(String),
    /// The JWT could not be split or its payload decoded into [`Claims`].
    #[error("jwt decode error: {0}")]
    Jwt(String),
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    fn sample_request() -> ExchangeRequest {
        ExchangeRequest {
            token_url: "https://sts.example/token".into(),
            subject_token: "ci.oidc.jwt".into(),
            audience: "anthropic-sts".into(),
            workspace_id: Some("ws_abc".into()),
            federation_rule_id: Some("rule_xyz".into()),
        }
    }

    fn b64url_no_pad(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b0 = u32::from(chunk[0]);
            let b1 = chunk.get(1).copied().map_or(0, u32::from);
            let b2 = chunk.get(2).copied().map_or(0, u32::from);
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
            if chunk.len() > 1 {
                out.push(ALPHABET[((n >> 6) & 63) as usize] as char);
            }
            if chunk.len() > 2 {
                out.push(ALPHABET[(n & 63) as usize] as char);
            }
        }
        out
    }

    #[test]
    fn form_contains_rfc8693_core_fields() {
        let form = build_exchange_form(&sample_request());
        let find = |k: &str| form.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(find("grant_type"), Some(GRANT_TYPE_TOKEN_EXCHANGE));
        assert_eq!(find("subject_token"), Some("ci.oidc.jwt"));
        assert_eq!(find("subject_token_type"), Some(SUBJECT_TOKEN_TYPE_JWT));
        assert_eq!(find("audience"), Some("anthropic-sts"));
    }

    #[test]
    fn form_includes_workspace_and_rule_when_present() {
        let form = build_exchange_form(&sample_request());
        assert!(form
            .iter()
            .any(|(k, v)| k == "workspace_id" && v == "ws_abc"));
        assert!(form
            .iter()
            .any(|(k, v)| k == "federation_rule_id" && v == "rule_xyz"));
    }

    #[test]
    fn form_omits_optionals_when_absent() {
        let mut req = sample_request();
        req.workspace_id = None;
        req.federation_rule_id = None;
        let form = build_exchange_form(&req);
        assert!(!form.iter().any(|(k, _)| k == "workspace_id"));
        assert!(!form.iter().any(|(k, _)| k == "federation_rule_id"));
        // The four mandatory fields remain.
        assert_eq!(form.len(), 4);
    }

    #[test]
    fn parse_token_response_ok() {
        let json = r#"{"access_token":"abc123","expires_in":3600,"token_type":"Bearer"}"#;
        let tok = parse_token_response(json).unwrap();
        assert_eq!(tok.access_token, "abc123");
        assert_eq!(tok.expires_in, 3600);
        assert_eq!(tok.token_type, "Bearer");
    }

    #[test]
    fn parse_token_response_accepts_string_expires_in() {
        let json = r#"{"access_token":"abc","expires_in":"900"}"#;
        let tok = parse_token_response(json).unwrap();
        assert_eq!(tok.expires_in, 900);
        // token_type defaults to Bearer when omitted.
        assert_eq!(tok.token_type, "Bearer");
    }

    #[test]
    fn parse_token_response_err_on_missing_access_token() {
        let json = r#"{"expires_in":60,"token_type":"Bearer"}"#;
        let err = parse_token_response(json).unwrap_err();
        assert!(matches!(err, OidcError::Parse(_)));
        // Also reject outright malformed JSON.
        assert!(matches!(
            parse_token_response("not json").unwrap_err(),
            OidcError::Parse(_)
        ));
    }

    #[test]
    fn parse_oidc_claims_decodes_known_payload() {
        let payload =
            br#"{"iss":"https://token.actions.githubusercontent.com","sub":"repo:acme/app:ref:refs/heads/main","aud":"anthropic","exp":1893456000}"#;
        let jwt = format!(
            "{}.{}.{}",
            b64url_no_pad(b"{\"alg\":\"RS256\"}"),
            b64url_no_pad(payload),
            "ZmFrZXNpZw"
        );
        let claims = parse_oidc_claims(&jwt).unwrap();
        assert_eq!(claims.iss, "https://token.actions.githubusercontent.com");
        assert_eq!(claims.sub, "repo:acme/app:ref:refs/heads/main");
        assert_eq!(claims.aud, "anthropic");
        assert_eq!(claims.exp, 1_893_456_000);
    }

    #[test]
    fn parse_oidc_claims_handles_array_aud() {
        let payload = br#"{"iss":"i","sub":"s","aud":["primary","secondary"],"exp":42}"#;
        let jwt = format!(
            "{}.{}.{}",
            b64url_no_pad(b"h"),
            b64url_no_pad(payload),
            b64url_no_pad(b"sig")
        );
        let claims = parse_oidc_claims(&jwt).unwrap();
        assert_eq!(claims.aud, "primary");
        assert_eq!(claims.exp, 42);
    }

    #[test]
    fn parse_oidc_claims_rejects_malformed_jwt() {
        // Wrong number of segments.
        assert!(matches!(
            parse_oidc_claims("only.two").unwrap_err(),
            OidcError::Jwt(_)
        ));
        // Three segments but payload is not valid base64url JSON.
        assert!(matches!(
            parse_oidc_claims("aaa.!!!.bbb").unwrap_err(),
            OidcError::Jwt(_)
        ));
    }

    #[test]
    fn is_expired_honors_skew() {
        let tok = ExchangedToken {
            access_token: "a".into(),
            expires_in: 1_000,
            token_type: "Bearer".into(),
        };
        // Well before deadline, no skew -> not expired.
        assert!(!is_expired(&tok, 900, 0));
        // 60s skew pushes the effective deadline earlier -> expired at 950.
        assert!(is_expired(&tok, 950, 60));
        // Exactly at deadline counts as expired.
        assert!(is_expired(&tok, 1_000, 0));
        // Saturating add: huge skew never panics.
        assert!(is_expired(&tok, u64::MAX, u64::MAX));
    }
}
