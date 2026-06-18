# origin-keyvault

> Cross-platform secret storage with a uniform async facade over per-OS keystores.

## Purpose

`origin-keyvault` stores provider credentials and OAuth tokens behind one async
facade, [`KeyVault`], dispatching to a platform-specific backend (Linux Secret
Service, macOS Keychain, Windows Credential Manager) or an in-process
[`MemoryBackend`] when `ORIGIN_KEYVAULT=memory`. Secrets cross the API only
through [`Secret<T>`], which zeroizes on drop and redacts in `Debug`. The crate
also ships an OAuth 2.0 PKCE auth-code + refresh driver and an append-only
[`audit::AuditRing`].

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `KeyVault` | struct | Facade: `detect`, `in_memory`, `set`/`get`/`delete`/`list`. |
| `KeyVault::detect_with_audit` / `with_audit` | fn | Attach an `AuditRing`. |
| `Secret<T>` | struct | Zeroize-on-drop, `Debug`-redacted secret wrapper. |
| `Error` | enum | `NotFound`, `Backend`, `Utf8`, `Serde`. |
| `OAuthClient` | struct | Auth-code `exchange` + `refresh` + `refresh_if_due`. |
| `Pkce` | struct | RFC 7636 verifier + S256 challenge pair. |
| `AuthCodeRequest`, `ExchangedTokens`, `RefreshOutcome` | type | OAuth payloads. |
| `audit::AuditRing` | struct | Page-rotating audit log; `record` / `replay`. |
| `audit::AuditAction` / `AuditEvent` | enum/struct | Get/Set/Delete/List records. |

## Key types

```rust
pub struct Secret<T: Zeroize> { inner: T }
impl<T: Zeroize> Secret<T> {
    pub const fn new(value: T) -> Self;
    pub const fn expose(&self) -> &T;          // callers must not log/persist
}
// no Clone, no Display, no Serialize; Debug prints "Secret<redacted>";
// Drop calls self.inner.zeroize().

impl KeyVault {
    pub fn detect() -> Result<Self, Error>;
    pub fn in_memory() -> Self;
    pub async fn set<T>(&self, provider: &str, account: &str, value: Secret<T>)
        -> Result<(), Error> where T: Zeroize + AsRef<[u8]> + Send;
    pub async fn get(&self, provider: &str, account: &str)
        -> Result<Secret<String>, Error>;
    pub async fn delete(&self, provider: &str, account: &str) -> Result<(), Error>;
    pub async fn list(&self, provider: &str) -> Result<Vec<String>, Error>;
}
```

The crate-private `Backend` trait (`set`/`get`/`delete`/`list` over `Vec<u8>`)
is what each per-OS implementation provides; the facade validates UTF-8 and
adapts results into `Secret<String>`.

## How it works

```text
KeyVault::detect()
   ├── ORIGIN_KEYVAULT=memory ──────► MemoryBackend (Arc<dyn Backend>)
   ├── cfg(linux)   ► LinuxBackend   (Secret Service / age fallback)
   ├── cfg(macos)   ► MacBackend     (Keychain via security-framework)
   ├── cfg(windows) ► WindowsBackend (Credential Manager, Cred*W FFI)
   └── otherwise    ► MemoryBackend
```

`set` materializes a short-lived `Vec<u8>` copy of the secret, calls the backend,
then `zeroize`s that buffer; the caller's `Secret<T>` zeroizes at function exit.
`get` wipes the intermediate buffer regardless of UTF-8 outcome. When an
`AuditRing` is attached, every method records a best-effort
`(provider, account, action, timestamp)` tuple after the backend call returns.

OAuth (`oauth.rs`) implements RFC 6749 §4.1 auth-code plus §6 refresh with PKCE:
`Pkce::new()` draws 96 bytes of CSPRNG, base64url-encodes a 128-char verifier and
its SHA-256 S256 challenge. `OAuthClient::exchange` swaps a code for tokens and
persists them through the vault under an `/oauth` account suffix so they never
collide with raw API keys; `refresh` / `refresh_if_due` rotate them.

## Dependencies & features

- `zeroize` (secret wiping), `parking_lot`, `async-trait`, `tokio`
  (`rt-multi-thread`, `sync`, `time`, `fs`, `io-util`), `reqwest` (rustls),
  `serde`/`serde_json`, `sha2`, `base64`, `rand`, `chrono`, `thiserror`.
- Per-OS deps: Linux `secret-service` + `age` + `dirs`; macOS `security-framework`
  (`>=3.5,<3.7` for MSRV); Windows `windows` (`Win32_Security_Credentials`).
- `unsafe_code = "allow"` (Windows `CredWriteW`/`CredReadW`/`CredDeleteW` FFI)
  with `undocumented_unsafe_blocks = "deny"`. No optional cargo features.

## Used by

`crates/*/Cargo.toml` matches for `origin-keyvault`:

- `crates/origin-browser/Cargo.toml`
- `crates/origin-cli/Cargo.toml`
- `crates/origin-daemon/Cargo.toml`
- `crates/origin-gmail/Cargo.toml`
- `crates/origin-keyvault/Cargo.toml`
- `crates/origin-mcp/Cargo.toml`
- `crates/origin-provider-github/Cargo.toml`

## Testing

Integration tests under `crates/origin-keyvault/tests/`: `audit.rs`,
`macos_keychain.rs`, `oauth_flow.rs`, `pkce.rs`, `round_trip.rs`. `wiremock`
mocks the OAuth token endpoint; `tempfile` backs the audit-ring tests. The
in-memory backend lets the round-trip and audit suites run on any host.

## See also

- [Security model](../security/security-model.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
