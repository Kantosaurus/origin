# Multi-Provider Expansion — Design

**Date:** 2026-05-20
**Status:** Draft, awaiting user review
**Scope:** Expand `origin`'s provider matrix from 7 to 37+ LLM providers (API-key and OAuth), via a catalog-driven architecture with a single generic OpenAI-compatible client.

## Goals

1. Support every provider the user listed (37 entries), plus a "Custom Provider" escape hatch.
2. No new boilerplate crate per provider. Adding the next provider must be **one row in a table**.
3. Honour the project rule: signature subsystems use novel mechanisms that beat openclaude/jcode/opencode on tokens or perf — we will *not* generate 30 near-identical crates.
4. Both API-key and OAuth flows work for every provider that offers them.
5. KeyVault stays the only crate that touches OS keychains; secrets never reach disk in plaintext.

## Non-goals

- Per-provider routing or cost optimisation (separate phase).
- Image / audio / embedding providers (user excluded these).
- Auto-discovery of model lists per provider.
- Breaking changes to the existing `Provider` trait or `ProviderFactory` public API.

## Provider taxonomy

The 37 providers decompose into five wire formats:

| Wire format | Providers (count) | Implementation |
|---|---|---|
| **OpenAI Chat Completions** | DeepSeek, Fireworks, Together AI, xAI (Grok), Mistral, Moonshot (Kimi K2.5), MiniMax, StepFun, Synthetic, Venice, Arcee AI, BytePlus, Chutes, Qwen Cloud, Qianfan, Volcano Engine, Xiaomi, Z.AI, Microsoft Foundry, LiteLLM, Vercel AI Gateway, Cloudflare AI Gateway, Kilo Gateway, OpenCode, Copilot Proxy, vLLM, SGLang, Hugging Face, OpenRouter, OpenAI (29) | One generic client (`origin-provider-openai-compat`) |
| **Anthropic Messages** | Anthropic (API key + Claude CLI OAuth) (1) | Existing `origin-provider-anthropic` |
| **Gemini `generateContent`** | Google (API key + Gemini CLI OAuth) (1) | Existing `origin-provider-gemini` |
| **Bedrock SigV4** | AWS Bedrock (1) | Existing `origin-provider-bedrock` |
| **Ollama NDJSON** | Ollama (1) | Existing `origin-provider-ollama` |
| **GitHub Copilot** | GitHub Copilot (OAuth) (1) | Existing `origin-provider-github` extended |
| **OpenAI Codex (ChatGPT OAuth)** | OpenAI Codex (1) | New module in `origin-provider-openai-compat`: same wire, OAuth bearer |
| **Custom** | user-defined (1) | catalog row from `~/.origin/providers.toml` |

Total: 37 providers, ~5 implementation surfaces.

## Architecture

### 1. `origin-provider::catalog`

New module in the existing `origin-provider` crate:

```rust
pub struct ProviderEntry {
    pub id: &'static str,              // "deepseek", "z-ai", "groq"
    pub display_name: &'static str,
    pub wire: WireFormat,
    pub auth: AuthScheme,
    pub base_url: &'static str,
    pub chat_path: &'static str,       // e.g. "/v1/chat/completions"
    pub default_model: &'static str,
    pub capabilities: Capabilities,
}

pub enum WireFormat {
    OpenAIChat,
    Anthropic,
    Gemini,
    Bedrock,
    Ollama,
    GitHubCopilot,
}

pub enum AuthScheme {
    None,
    ApiKey { header: &'static str, prefix: &'static str },
    OAuth(OAuthSpec),
    SigV4 { service: &'static str },
    Custom,
}

pub struct OAuthSpec {
    pub authorize_url: &'static str,
    pub token_url: &'static str,
    pub client_id: &'static str,
    pub scopes: &'static [&'static str],
    pub redirect_uri: &'static str,
    pub pkce: bool,
    pub device_flow: bool,
}

pub struct Capabilities {
    pub streaming: bool,
    pub tools: bool,
    pub prompt_cache: bool,
    pub thinking: bool,
}

pub const BUILTIN_CATALOG: &[ProviderEntry] = &[ /* 37 rows */ ];
```

The static catalog plus the merged user-defined catalog (see §4) drives every later decision: parsing, auth lookup, factory construction, CLI listing.

### 2. `origin-provider-openai-compat`

New crate. Extracts the body of the existing `origin-provider-openai` into a parameterised client:

```rust
pub struct OpenAiCompat {
    base_url: String,
    chat_path: String,
    auth: AuthSource,         // ApiKey { header, prefix, value } | OAuthToken(Arc<dyn TokenSource>)
    extra_headers: Vec<(String, String)>,
    client: reqwest::Client,
}

impl Provider for OpenAiCompat { /* delegates to the existing wire::encode_request + sse pump */ }
```

`origin-provider-openai` and `origin-provider-openrouter` become 30-line wrappers that construct `OpenAiCompat` with their specific config. Behaviour is preserved; existing tests (including the `openai_stream` fuzz target) keep passing without modification.

The `TokenSource` trait lets OAuth providers (Codex, GitHub Copilot via the chat-completions endpoint) inject a refreshed bearer per request without entangling the wire layer with vault logic.

### 3. `ProviderFactory` rewrite

`crates/origin-daemon/src/provider_factory.rs`:

- `ProviderId` becomes `pub struct ProviderId(Cow<'static, str>)` — carries the catalog id directly. `parse()` succeeds iff the id exists in the merged catalog.
- `build(id, account)` looks up the catalog row, then:
  - `WireFormat::OpenAIChat` → `OpenAiCompat::new(entry, auth_value_from_vault)`
  - `WireFormat::Anthropic` → existing `Anthropic::new(...)`
  - `WireFormat::Gemini` → existing `Gemini::new(...)`, with OAuth-token path if `entry.auth == OAuth(_)`
  - `WireFormat::Bedrock` → existing `Bedrock::new(...)`
  - `WireFormat::Ollama` → `Ollama::new()`
  - `WireFormat::GitHubCopilot` → existing `GitHubModels::new(vault, account)` (token-swap stays internal)
- For `AuthScheme::OAuth`, build calls `OAuthClient::refresh_if_due` against the entry's `OAuthSpec` before constructing the client.

### 4. Custom providers

`~/.origin/providers.toml` (path overridable by `--config`) is loaded at daemon startup:

```toml
[providers.my-gateway]
display_name = "My Gateway"
wire = "openai-chat"
base_url = "https://gw.example.com"
chat_path = "/v1/chat/completions"
default_model = "llama-3.3-70b"
auth = { kind = "api-key", header = "Authorization", prefix = "Bearer " }
```

Custom rows merge on top of `BUILTIN_CATALOG`. A user-defined id colliding with a builtin id is a startup error (loud failure, not silent override) to prevent unintentional shadowing.

### 5. OAuth flows

The four OAuth-only providers ship `OAuthSpec` rows that drive `origin-keyvault::OAuthClient`:

| Provider id | Auth flow | Notes |
|---|---|---|
| `anthropic-oauth` | PKCE auth-code via Claude.ai pairing | Existing impl, exposed as a catalog row |
| `gemini-oauth` | Google OAuth (PKCE) | Uses Gemini wire format with OAuth bearer instead of API key |
| `github-copilot` | GitHub device flow + Copilot token swap | Existing `GitHubModels`, refactored to read `OAuthSpec` from catalog |
| `openai-codex` | ChatGPT (Codex) OAuth | OpenAI-compat wire + OAuth bearer instead of API key |

Refresh tokens stay in `KeyVault` under `({provider_id}, "{account}/oauth")` — already the existing layout. No vault-schema change.

### 6. CLI surface

- `origin keyring set <provider>:<account>` — unchanged; works for any API-key provider in the catalog.
- `origin keyring login <provider>:<account>` — unchanged; the catalog's `OAuthSpec` drives the browser/device flow.
- `origin keyring list` — unchanged.
- `origin providers list` — **new**. Prints the catalog with `id | display_name | wire | auth | capabilities`.
- `origin providers describe <id>` — **new**. Prints one entry's full config (model id, base URL, chat path, capabilities).
- `origin run --provider <id> --account <name>` — unchanged; accepts any catalog id.

### 7. Default-build feature surface

| Feature | Default? | Pulls in |
|---|---|---|
| `openai-compat` | yes | The generic client + all OpenAI-compatible catalog rows |
| `anthropic` | yes | Anthropic API + OAuth |
| `gemini` | yes | Gemini API + OAuth |
| `ollama` | yes | Ollama |
| `openrouter` | yes (now) | OpenRouter (uses openai-compat client) |
| `bedrock` | opt-in | SigV4 dependency |
| `github-models` | opt-in | GitHub OAuth |
| `oauth-providers` | yes | OAuth wiring for catalog rows (Codex, Claude OAuth, Gemini OAuth, GH Copilot) |
| `custom-providers` | yes | `providers.toml` loader |

A default build covers all 37 providers except Bedrock (kept opt-in to avoid pulling SigV4).

## Data flow

```
user → origin-cli (--provider <id>)
       → daemon
         → ProviderFactory::build(id, account)
           → catalog.lookup(id) → ProviderEntry
           → vault.get(id, account) | OAuthClient::refresh_if_due(id, account, vault)
           → match entry.wire {
               OpenAIChat   → OpenAiCompat::new(entry, auth_value)
               Anthropic    → Anthropic::new(auth_value).with_cas(cas)
               Gemini       → Gemini::new(auth_value)
               Bedrock      → Bedrock::new(...)
               Ollama       → Ollama::new()
               GitHubCopilot→ GitHubModels::new(vault, account)
             }
         → Provider trait object → existing chat / chat_stream paths
```

## Testing strategy

- **Catalog invariants (unit, fast):**
  - Every `id` is unique in `BUILTIN_CATALOG`.
  - Every `id` round-trips through `ProviderId::parse`.
  - Every `base_url` parses as a valid URL.
  - Every `OAuthSpec` has non-empty `authorize_url`, `token_url`, `client_id`.
- **Wire tests (`wiremock`, fast):** one happy-path test per wire format (OpenAIChat, Anthropic, Gemini, Bedrock, Ollama, GitHubCopilot). The generic OpenAIChat test parameterises over 4–5 representative quirky providers (Cloudflare URL templating, Azure deployment path, OpenRouter required headers, custom auth-header providers).
- **OAuth tests:** PKCE round-trip + refresh per OAuth provider against a mock token endpoint. Extends the existing `origin-keyvault::oauth` tests.
- **Custom-provider tests:** load a fixture `providers.toml`; assert merged catalog contains the row and that `ProviderFactory::build` succeeds against a `wiremock` stub.
- **No-regression:** existing `anthropic_smoke`, `openai_stream` fuzz target, `memory_e2e` round-trip tests must keep passing.

## Migration

The existing `OpenAi` and `OpenRouter` provider types stay as public re-exports, now constructed as thin wrappers around `OpenAiCompat`. The existing `ProviderId::OpenAi`, `Anthropic`, … variants are kept as `const` aliases for source compatibility:

```rust
pub const OPENAI: ProviderId = ProviderId::new_static("openai");
```

`Self::OpenAi` references in the daemon become `ProviderId::OPENAI` (or unchanged, behind a tiny `const fn` shim). Tests are updated mechanically.

## Risk register

| Risk | Mitigation |
|---|---|
| Provider X has a quirk the OpenAI-compat client can't express (e.g. nonstandard URL templating) | URL template supports `{account_id}`, `{project_id}`, `{deployment}` placeholders resolved from the vault credential blob. Anything beyond that lands in a small per-provider shim or stays as a "Custom" row. |
| OAuth client IDs / redirect URIs require registration | Document required registration in `docs/site/src/providers.md` per OAuth row. Ship reasonable defaults where the provider publishes a public client id (Anthropic OAuth, Gemini CLI, GitHub). |
| Stream parsing varies per provider (some emit `[DONE]`, some don't) | Existing `origin_provider::sse` pump already handles the common patterns. Edge cases land as per-entry `quirks` flags. |
| `cargo build` time grows | Adding one generic crate + 37 catalog rows is cheaper than 30 new crates. We measured the existing `origin-provider-openai` at ~3s clean — the generic client is bounded by that. |
| Static catalog drift vs reality | A `xtask provider-doctor` command pings each provider's `/v1/models` (where available) and asserts the canonical model id is still served. Run in CI weekly, not per-PR. |

## Spec self-review (filled inline)

- **Placeholders:** none.
- **Internal consistency:** §1 says 37 providers; §"Provider taxonomy" table sums to 37 (29 OpenAI-compat including OpenAI + OpenRouter, 1 Anthropic, 1 Gemini, 1 Bedrock, 1 Ollama, 1 GH Copilot, 1 Codex, 1 Custom — Anthropic-OAuth and Gemini-OAuth are catalog variants of the same wire format, not new wires). Verified.
- **Scope check:** Single mechanism (catalog + generic client + OAuth wiring), scoped for one implementation plan.
- **Ambiguity check:** "Custom Provider" semantics fully specified in §4; collision behaviour explicit (loud failure).
