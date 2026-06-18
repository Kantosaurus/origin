# origin-oidc

> Workload Identity Federation / OIDC token-exchange request building and response parsing for keyless CI auth

## Purpose

`origin-oidc` adds keyless Workload Identity Federation (WIF) so CI runners no
longer leak a long-lived API secret. It builds the RFC 8693 token-exchange request
and parses the STS response — a pure request-builder + response-parser. The HTTP
POST stays the caller's job, so the crate has no network, no async, and no extra
base64 dependency (the JWT payload is decoded with a hand-rolled base64url table).

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `ExchangeRequest` | struct | Inputs for a WIF token exchange (token URL, subject token, audience, optional workspace/rule ids). |
| `build_exchange_form` | fn | Build the unencoded `application/x-www-form-urlencoded` field pairs. |
| `ExchangedToken` | struct | A successfully exchanged short-lived token. |
| `parse_token_response` | fn | Parse the STS JSON response into an `ExchangedToken`. |
| `Claims` | struct | Decoded JWT claims (`iss`, `sub`, `aud`, `exp`). |
| `parse_oidc_claims` | fn | Decode a JWT payload's claims (no signature verification). |
| `is_expired` | fn | Whether a token is expired at `now_unix`, with early-expiry skew. |
| `GRANT_TYPE_TOKEN_EXCHANGE` / `SUBJECT_TOKEN_TYPE_JWT` | const | RFC 8693 grant + subject-token type URNs. |
| `OidcError` | enum | `Parse(String)` / `Jwt(String)`. |

## Key types

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeRequest {
    pub token_url: String,
    pub subject_token: String,
    pub audience: String,
    pub workspace_id: Option<String>,
    pub federation_rule_id: Option<String>,
}

pub fn build_exchange_form(req: &ExchangeRequest) -> Vec<(String, String)>;
pub fn parse_token_response(json: &str) -> Result<ExchangedToken, OidcError>;
pub fn parse_oidc_claims(jwt: &str) -> Result<Claims, OidcError>;
pub fn is_expired(tok: &ExchangedToken, now_unix: u64, skew_secs: u64) -> bool;
```

## How it works

**Request building.** `build_exchange_form` emits the RFC 8693 fields in a stable
order — `grant_type` (`urn:ietf:params:oauth:grant-type:token-exchange`),
`subject_token`, `subject_token_type`
(`urn:ietf:params:oauth:token-type:jwt`), `audience`, then the optional
`workspace_id` (`ANTHROPIC_WORKSPACE_ID`) and `federation_rule_id`
(`anthropic_federation_rule_id`) when present. The pairs are unencoded; the caller
URL-encodes and POSTs them to `token_url`.

**Response parsing.** `parse_token_response` reads a loose `RawTokenResponse`:
`access_token` is required (empty/missing → `OidcError::Parse`), `expires_in` is
coerced from a number or numeric string (defaulting to 0), and `token_type`
defaults to `Bearer`.

**JWT inspection.** `parse_oidc_claims` base64url-decodes the middle (payload)
segment of a `header.payload.signature` JWT and reads `iss`, `sub`, `aud`, `exp`.
It performs **no** signature verification (that is the IdP's job); `aud` may be a
string or the first element of an array. Decoding uses an inline base64url table
(`base64url_decode` / `base64url_value`), so there is no base64 dependency.

**Expiry.** `is_expired(tok, now_unix, skew_secs)` treats `expires_in` as an
absolute deadline and reports expired when `now_unix + skew_secs >= expires_in`,
using a saturating add so a huge skew never panics.

## Dependencies & features

Only `serde` (derive), `serde_json`, and `thiserror`. No async, no network, no
other origin crates. No cargo features.

## Used by

`Grep "origin-oidc"` over `crates/*/Cargo.toml`:

```
crates/origin-cli/Cargo.toml
crates/origin-daemon/Cargo.toml
crates/origin-oidc/Cargo.toml
```

## Testing

All coverage is in-file (`#[cfg(test)] mod tests`): the RFC 8693 core fields and
optional workspace/rule inclusion/omission, token-response parsing (numeric and
string `expires_in`, missing `access_token`, malformed JSON), JWT claim decoding
(known payload, array `aud`, malformed JWT rejection), and `is_expired` skew /
boundary / saturating-add behaviour.

## See also

- [Providers subsystem](../subsystems/providers.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
