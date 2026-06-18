# origin-provider-github

> GitHub Copilot provider for origin with vault-sourced OAuth session tokens

## Purpose

`origin-provider-github` provides the GitHub **Copilot** chat provider. Copilot's
chat API is OpenAI-shaped, so the crate reuses
[`origin_provider_openai_compat::OpenAiCompat`] for the actual chat/streaming/tool
wire, and supplies a custom `TokenSource` that mints and caches a short-lived
Copilot *session token* by exchanging the stored GitHub OAuth token, plus the
editor-identity headers Copilot validates.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `copilot::provider` | fn | Build a Copilot `OpenAiCompat` provider for an account's vault credentials. |
| `copilot::provider_with_base` | fn | Same, against an explicit chat base URL (testing / enterprise). |

## Key types

```rust
// copilot.rs
pub fn provider(vault: KeyVault, account: impl Into<String>) -> OpenAiCompat;

pub fn provider_with_base(
    vault: KeyVault,
    account: impl Into<String>,
    base: &str,
) -> OpenAiCompat;
```

The returned value is an `OpenAiCompat` configured with `name = "github-copilot"`,
a `CopilotTokenSource` (private), and the required editor headers.

## How it works

**Wire API.** Copilot chat is plain OpenAI Chat-Completions
(`POST /chat/completions`), so all encode/decode/streaming/tool handling is the
`OpenAiCompat` client. The default chat host is
`https://api.individual.githubcopilot.com` (business/enterprise plans differ; the
token response's `endpoints.api` is authoritative).

**Auth (session-token exchange).** The chat request is *not* called with the raw
GitHub OAuth token. The private `CopilotTokenSource` (a `TokenSource` impl):

1. Reads the stored GitHub OAuth token from the `origin-keyvault` `KeyVault`
   (`get("github-copilot", "{account}/oauth")`).
2. Exchanges it for a Copilot session token at
   `GET api.github.com/copilot_internal/v2/token`
   (overridable via `ORIGIN_COPILOT_TOKEN_URL` for tests).
3. Caches the session token and refreshes it `EXPIRY_MARGIN_SECS` (60s) before
   its stated expiry, so an in-flight request never races the boundary. The cache
   mutex is never held across an `await`.
4. Returns `Authorization: Bearer <session token>`.

**Required headers.** The provider always sends the editor-identity headers
Copilot checks for presence: `Copilot-Integration-Id: vscode-chat`,
`Editor-Version`, `Editor-Plugin-Version`, and a `GithubCopilot/…` `User-Agent`.

A separate `GitHub Models` (`models.github.ai`) provider once lived here but was
removed as dead code; `github` / `github-models` ids alias to `github-copilot`.

## Dependencies & features

`origin-keyvault` (credential storage) and `origin-provider-openai-compat` (the
chat wire); `reqwest` (rustls, no `stream` — streaming is delegated to
`OpenAiCompat`), `async-trait`, `serde`/`serde_json`. `origin-core` and
`origin-provider` are dev-dependencies for the integration tests. No extra cargo
features.

## Used by

`Grep "origin-provider-github"` over `crates/*/Cargo.toml`:

```
crates/origin-daemon/Cargo.toml
crates/origin-provider-github/Cargo.toml
```

## Testing

`tests/copilot.rs` exercises the session-token exchange and header wiring with
`wiremock`, redirecting the exchange endpoint via `ORIGIN_COPILOT_TOKEN_URL`.

## See also

- [Providers subsystem](../subsystems/providers.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
