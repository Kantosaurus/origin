# origin-gmail

> First-class Gmail tool over Google OAuth2 (auth-code + refresh) with token-frugal metadata reads

## Purpose

`origin-gmail` exposes the user's Gmail to the agent as a read-only tool. It
authenticates with Google's OAuth 2.0 authorization-code grant's **refresh-token**
leg — loading `refresh_token`, `client_id`, and `client_secret` from
`origin-keyvault`, exchanging them for a short-lived bearer — then calls the
Gmail REST API v1 to search messages, fetch a message's metadata and best-effort
body, and list threads. It is deliberately token-frugal so a triage view costs a
fraction of a full fetch.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Gmail` | struct | The client holding a minted bearer; no `Debug` (carries a token). |
| `Gmail::from_keyvault` / `from_keyvault_at` | fn | Load creds from `("google","gmail")` (or explicit) and refresh a token. |
| `Gmail::from_access_token` | fn | Build from a pre-minted token (tests / external OAuth). |
| `Gmail::search` / `search_page` | fn | Find messages by Gmail query, with paging. |
| `Gmail::get_message` / `get_message_full` | fn | Fetch metadata (default) or the full body. |
| `Gmail::list_threads` / `list_threads_page` | fn | List threads matching a query, with paging. |
| `Credentials` | struct | Parsed `{client_id, client_secret, refresh_token}` (secrets zeroized). |
| `Message` / `MessageRef` / `ThreadRef` / `Header` / `Page` | struct | Typed model values. |
| `Error` / `Result` | enum/alias | `Credentials` / `CredentialFormat` / HTTP error variants. |
| `DEFAULT_PROVIDER` / `DEFAULT_ACCOUNT` / `DEFAULT_MAX` | const | `"google"` / `"gmail"` / `25`. |

## Key types

```rust
pub struct Gmail { http: http::HttpClient }   // no Debug: holds a bearer token

impl Gmail {
    pub async fn from_keyvault(vault: &KeyVault) -> Result<Self>;
    pub async fn search(&self, query: &str, max: u32) -> Result<Vec<MessageRef>>;
    pub async fn get_message(&self, id: &str) -> Result<Message>;      // format=metadata
    pub async fn get_message_full(&self, id: &str) -> Result<Message>; // format=full
    pub async fn list_threads(&self, query: &str, max: u32) -> Result<Vec<ThreadRef>>;
}
```

## How it works

The crate is a pure state machine with the network injected at one seam:
`request` builds URLs and form bodies (pure, unit-tested), `model` parses API
JSON into typed values (pure, unit-tested), and `http` is the *only* module that
touches the network. `from_keyvault` reads the credential blob via the vault,
parses it with `Credentials::from_json`, then POSTs `refresh_form` to the token
endpoint to mint an access token, which `HttpClient` carries on every call.
Token frugality is structural: `get_message` defaults to `format=metadata` with
a tight `metadataHeaders` allow-list, and every list call carries an explicit
`maxResults` cap and pages lazily via continuation tokens.

```
KeyVault("google","gmail") ─▶ Credentials ─▶ refresh_form ─POST─▶ access_token
                                                                      │
search/get/list ─▶ request(url, params) ─▶ http (bearer) ─▶ model::parse ─▶ typed
```

## Dependencies & features

No cargo features. `origin-keyvault` stores creds and holds every secret in
`Secret<String>` (zeroized on drop, redacted in `Debug`). `reqwest` is the HTTP
client; `serde`/`serde_json` model the API; `base64` and `sha2` support the
OAuth/encoding paths; `tokio` provides the async runtime.

## Used by

```
crates/origin-cli/Cargo.toml
crates/origin-daemon/Cargo.toml
crates/origin-gmail/Cargo.toml
crates/origin-tools/Cargo.toml
```

## Testing

Because `request` and `model` are pure, almost everything is tested without a
live Google connection: URL/form builders, the metadata header allow-list, and
JSON→typed parsing (`Message::from_value`, refs, paging). The `gmail` builtin in
`origin-tools` carries a drift-guard test that asserts its inlined schema matches
the values this crate emits. `tokio` test utilities support the async paths.

## See also

- [Tools subsystem](../subsystems/tools.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
