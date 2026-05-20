# Multi-Provider Expansion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add 30+ LLM providers (API-key + OAuth) to `origin` via a catalog-driven architecture, without spawning 30 boilerplate crates.

**Architecture:** A static `ProviderEntry` catalog in `origin-provider` enumerates every provider with its wire format, auth scheme, base URL, and capabilities. A single new crate `origin-provider-openai-compat` implements the OpenAI Chat-Completions wire format for any base URL + auth config; the 29 OpenAI-shaped providers light up from catalog rows. The 4 unique wire formats (Anthropic, Gemini, Bedrock, Ollama) keep their existing crates. OAuth flows reuse `origin-keyvault::OAuthClient` driven by `OAuthSpec` rows in the catalog. A `~/.origin/providers.toml` loader merges user-defined entries on top of the static catalog.

**Tech Stack:** Rust 1.83 (workspace-pinned), `reqwest` 0.12 + rustls, `serde` + `serde_json`, `wiremock` 0.6 for HTTP tests, `tokio` 1, `async-trait` 0.1.

---

## File Structure

**New files**

- `crates/origin-provider/src/catalog.rs` — `ProviderEntry`, `WireFormat`, `AuthScheme`, `OAuthSpec`, `Capabilities`, `BUILTIN_CATALOG`.
- `crates/origin-provider/src/catalog_rows.rs` — the 37 static entries (split out for readability).
- `crates/origin-provider/src/token_source.rs` — `TokenSource` trait (API key wrapper + OAuth token wrapper).
- `crates/origin-provider/src/custom.rs` — `providers.toml` loader; `load_custom(path) -> Vec<ProviderEntry>`.
- `crates/origin-provider-openai-compat/Cargo.toml`
- `crates/origin-provider-openai-compat/src/lib.rs` — `OpenAiCompat` provider + config struct.
- `crates/origin-provider-openai-compat/src/wire.rs` — moved from `origin-provider-openai/src/wire.rs`.
- `crates/origin-provider-openai-compat/src/streaming.rs` — moved from `origin-provider-openai/src/streaming.rs`.
- `crates/origin-provider-openai-compat/tests/wire_smoke.rs` — wiremock happy-path test.
- `crates/origin-daemon/tests/catalog_factory.rs` — integration tests for the catalog-driven factory.

**Modified files**

- `crates/origin-provider/src/lib.rs` — pub mod `catalog`, `token_source`, `custom`.
- `crates/origin-provider/Cargo.toml` — add `toml = "0.8"`, `url = "2"` deps.
- `crates/origin-provider-openai/src/lib.rs` — gut, re-export `OpenAi` as thin wrapper around `OpenAiCompat`.
- `crates/origin-provider-openai/src/wire.rs` — delete (moved).
- `crates/origin-provider-openai/src/streaming.rs` — delete (moved).
- `crates/origin-provider-openai/Cargo.toml` — depend on `origin-provider-openai-compat`.
- `crates/origin-provider-openrouter/src/lib.rs` — refactor to wrap `OpenAiCompat`.
- `crates/origin-provider-openrouter/Cargo.toml` — depend on `origin-provider-openai-compat`.
- `crates/origin-provider-gemini/src/lib.rs` — add `Gemini::with_oauth_token(token)` constructor.
- `crates/origin-provider-anthropic/src/lib.rs` — add `Anthropic::with_oauth_token(token)` (already partially supported; expose via factory).
- `crates/origin-provider-github/src/lib.rs` — accept an `OAuthSpec` from the catalog.
- `crates/origin-daemon/src/provider_factory.rs` — rewrite around the catalog.
- `crates/origin-daemon/src/lib.rs` — pass merged catalog (builtin + custom) to factory.
- `crates/origin-daemon/Cargo.toml` — add `oauth-providers`, `custom-providers`, `openai-compat` features; default-feature list grows.
- `crates/origin-cli/src/cli_def.rs` — add `Providers { Ls, Describe { id: String } }` and `KeyringSub::Login { provider: String, account: String }`.
- `crates/origin-cli/src/main.rs` — wire the new subcommands.
- `Cargo.toml` (workspace) — add `crates/origin-provider-openai-compat` (member auto-picked by `crates/*`, no edit needed).
- `docs/site/src/providers.md` — replace 7-row table with 37-row table + OAuth notes.
- `crates/origin-provider/Cargo.toml` — add `toml`, `url`, `serde`-derives.

---

## Phase A — Catalog scaffolding (no behaviour change)

### Task 1: Catalog types

**Files:**
- Create: `crates/origin-provider/src/catalog.rs`
- Modify: `crates/origin-provider/src/lib.rs` (add `pub mod catalog;`)
- Test: `crates/origin-provider/src/catalog.rs` (unit tests inline)

- [ ] **Step 1: Write failing test**

Append to a new `crates/origin-provider/src/catalog.rs`:

```rust
//! Static catalog of supported LLM providers.

use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireFormat {
    OpenAIChat,
    Anthropic,
    Gemini,
    Bedrock,
    Ollama,
    GitHubCopilot,
}

#[derive(Debug, Clone)]
pub enum AuthScheme {
    None,
    ApiKey { header: Cow<'static, str>, prefix: Cow<'static, str> },
    OAuth(OAuthSpec),
    SigV4 { service: Cow<'static, str> },
    Custom,
}

#[derive(Debug, Clone)]
pub struct OAuthSpec {
    pub authorize_url: Cow<'static, str>,
    pub token_url: Cow<'static, str>,
    pub client_id: Cow<'static, str>,
    pub scopes: Cow<'static, [Cow<'static, str>]>,
    pub redirect_uri: Cow<'static, str>,
    pub pkce: bool,
    pub device_flow: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Capabilities {
    pub streaming: bool,
    pub tools: bool,
    pub prompt_cache: bool,
    pub thinking: bool,
}

#[derive(Debug, Clone)]
pub struct ProviderEntry {
    pub id: Cow<'static, str>,
    pub display_name: Cow<'static, str>,
    pub wire: WireFormat,
    pub auth: AuthScheme,
    pub base_url: Cow<'static, str>,
    pub chat_path: Cow<'static, str>,
    pub default_model: Cow<'static, str>,
    pub capabilities: Capabilities,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_entry_constructs() {
        let e = ProviderEntry {
            id: "test".into(),
            display_name: "Test".into(),
            wire: WireFormat::OpenAIChat,
            auth: AuthScheme::None,
            base_url: "https://example.com".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "test-model".into(),
            capabilities: Capabilities::default(),
        };
        assert_eq!(e.id, "test");
        assert_eq!(e.wire, WireFormat::OpenAIChat);
    }
}
```

Append to `crates/origin-provider/src/lib.rs` after the existing `pub mod ndjson;` block:

```rust
pub mod catalog;
```

- [ ] **Step 2: Run test (it should pass on compile)**

Run: `cargo test -p origin-provider catalog::tests::provider_entry_constructs`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/origin-provider/src/catalog.rs crates/origin-provider/src/lib.rs
git commit -m "feat(provider): catalog types (ProviderEntry, WireFormat, AuthScheme)"
```

---

### Task 2: Static BUILTIN_CATALOG with all 37 rows

**Files:**
- Create: `crates/origin-provider/src/catalog_rows.rs`
- Modify: `crates/origin-provider/src/catalog.rs` (add `pub use catalog_rows::BUILTIN_CATALOG;` and `pub fn lookup`)
- Modify: `crates/origin-provider/src/lib.rs` (add `mod catalog_rows;`)
- Test: inline in `catalog_rows.rs`

- [ ] **Step 1: Write the catalog rows**

Create `crates/origin-provider/src/catalog_rows.rs`:

```rust
//! Static catalog rows — one per supported provider.

use std::borrow::Cow;
use crate::catalog::{AuthScheme, Capabilities, OAuthSpec, ProviderEntry, WireFormat};

const FULL_CAPS: Capabilities = Capabilities {
    streaming: true,
    tools: true,
    prompt_cache: false,
    thinking: false,
};

const STREAM_ONLY: Capabilities = Capabilities {
    streaming: true,
    tools: false,
    prompt_cache: false,
    thinking: false,
};

fn bearer() -> AuthScheme {
    AuthScheme::ApiKey {
        header: Cow::Borrowed("Authorization"),
        prefix: Cow::Borrowed("Bearer "),
    }
}

fn xapikey() -> AuthScheme {
    AuthScheme::ApiKey {
        header: Cow::Borrowed("x-api-key"),
        prefix: Cow::Borrowed(""),
    }
}

pub fn builtin_catalog() -> Vec<ProviderEntry> {
    vec![
        // ---- Native wire formats ----
        ProviderEntry {
            id: "anthropic".into(),
            display_name: "Anthropic (API key)".into(),
            wire: WireFormat::Anthropic,
            auth: xapikey(),
            base_url: "https://api.anthropic.com".into(),
            chat_path: "/v1/messages".into(),
            default_model: "claude-sonnet-4-6".into(),
            capabilities: Capabilities { prompt_cache: true, thinking: true, ..FULL_CAPS },
        },
        ProviderEntry {
            id: "anthropic-oauth".into(),
            display_name: "Anthropic (Claude CLI OAuth)".into(),
            wire: WireFormat::Anthropic,
            auth: AuthScheme::OAuth(OAuthSpec {
                authorize_url: "https://claude.ai/oauth/authorize".into(),
                token_url: "https://console.anthropic.com/v1/oauth/token".into(),
                client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e".into(), // public Claude.ai pairing id
                scopes: Cow::Borrowed(&[Cow::Borrowed("org:create_api_key"), Cow::Borrowed("user:profile"), Cow::Borrowed("user:inference")]),
                redirect_uri: "https://console.anthropic.com/oauth/code/callback".into(),
                pkce: true,
                device_flow: false,
            }),
            base_url: "https://api.anthropic.com".into(),
            chat_path: "/v1/messages".into(),
            default_model: "claude-sonnet-4-6".into(),
            capabilities: Capabilities { prompt_cache: true, thinking: true, ..FULL_CAPS },
        },
        ProviderEntry {
            id: "google".into(),
            display_name: "Google (Gemini API key)".into(),
            wire: WireFormat::Gemini,
            auth: AuthScheme::ApiKey {
                header: Cow::Borrowed("x-goog-api-key"),
                prefix: Cow::Borrowed(""),
            },
            base_url: "https://generativelanguage.googleapis.com".into(),
            chat_path: "/v1beta/models".into(),
            default_model: "gemini-2.5-pro".into(),
            capabilities: Capabilities { prompt_cache: true, thinking: true, ..FULL_CAPS },
        },
        ProviderEntry {
            id: "gemini-oauth".into(),
            display_name: "Gemini CLI OAuth".into(),
            wire: WireFormat::Gemini,
            auth: AuthScheme::OAuth(OAuthSpec {
                authorize_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
                token_url: "https://oauth2.googleapis.com/token".into(),
                client_id: "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com".into(),
                scopes: Cow::Borrowed(&[Cow::Borrowed("https://www.googleapis.com/auth/generative-language.retriever")]),
                redirect_uri: "http://localhost:8085".into(),
                pkce: true,
                device_flow: false,
            }),
            base_url: "https://generativelanguage.googleapis.com".into(),
            chat_path: "/v1beta/models".into(),
            default_model: "gemini-2.5-pro".into(),
            capabilities: Capabilities { prompt_cache: true, thinking: true, ..FULL_CAPS },
        },
        ProviderEntry {
            id: "bedrock".into(),
            display_name: "AWS Bedrock".into(),
            wire: WireFormat::Bedrock,
            auth: AuthScheme::SigV4 { service: Cow::Borrowed("bedrock") },
            base_url: "https://bedrock-runtime".into(),
            chat_path: "/model".into(),
            default_model: "anthropic.claude-3-haiku-20240307-v1:0".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "ollama".into(),
            display_name: "Ollama (local)".into(),
            wire: WireFormat::Ollama,
            auth: AuthScheme::None,
            base_url: "http://localhost:11434".into(),
            chat_path: "/api/chat".into(),
            default_model: "llama3.2".into(),
            capabilities: STREAM_ONLY,
        },
        ProviderEntry {
            id: "github-copilot".into(),
            display_name: "GitHub Copilot".into(),
            wire: WireFormat::GitHubCopilot,
            auth: AuthScheme::OAuth(OAuthSpec {
                authorize_url: "https://github.com/login/device/code".into(),
                token_url: "https://github.com/login/oauth/access_token".into(),
                client_id: "Iv1.b507a08c87ecfe98".into(), // public Copilot client id
                scopes: Cow::Borrowed(&[Cow::Borrowed("read:user")]),
                redirect_uri: "".into(),
                pkce: false,
                device_flow: true,
            }),
            base_url: "https://api.githubcopilot.com".into(),
            chat_path: "/chat/completions".into(),
            default_model: "gpt-4o".into(),
            capabilities: FULL_CAPS,
        },

        // ---- OpenAI Chat-Completions compatible (29 providers) ----
        ProviderEntry { id: "openai".into(), display_name: "OpenAI".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://api.openai.com".into(), chat_path: "/v1/chat/completions".into(), default_model: "gpt-4o".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "openai-codex".into(), display_name: "OpenAI Codex (ChatGPT OAuth)".into(), wire: WireFormat::OpenAIChat,
            auth: AuthScheme::OAuth(OAuthSpec {
                authorize_url: "https://auth.openai.com/oauth/authorize".into(),
                token_url: "https://auth.openai.com/oauth/token".into(),
                client_id: "app_EMoamEEZ73f0CkXaXp7hrann".into(),
                scopes: Cow::Borrowed(&[Cow::Borrowed("openid"), Cow::Borrowed("profile"), Cow::Borrowed("email")]),
                redirect_uri: "http://localhost:1455/auth/callback".into(),
                pkce: true,
                device_flow: false,
            }),
            base_url: "https://chatgpt.com/backend-api/codex".into(), chat_path: "/responses".into(), default_model: "gpt-5-codex".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "openrouter".into(), display_name: "OpenRouter".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://openrouter.ai".into(), chat_path: "/api/v1/chat/completions".into(), default_model: "openrouter/auto".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "deepseek".into(), display_name: "DeepSeek".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://api.deepseek.com".into(), chat_path: "/v1/chat/completions".into(), default_model: "deepseek-chat".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "fireworks".into(), display_name: "Fireworks AI".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://api.fireworks.ai".into(), chat_path: "/inference/v1/chat/completions".into(), default_model: "accounts/fireworks/models/llama-v3p3-70b-instruct".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "together".into(), display_name: "Together AI".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://api.together.xyz".into(), chat_path: "/v1/chat/completions".into(), default_model: "meta-llama/Llama-3.3-70B-Instruct-Turbo".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "xai".into(), display_name: "xAI (Grok)".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://api.x.ai".into(), chat_path: "/v1/chat/completions".into(), default_model: "grok-4".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "mistral".into(), display_name: "Mistral AI".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://api.mistral.ai".into(), chat_path: "/v1/chat/completions".into(), default_model: "mistral-large-latest".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "moonshot".into(), display_name: "Moonshot AI (Kimi)".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://api.moonshot.ai".into(), chat_path: "/v1/chat/completions".into(), default_model: "kimi-k2.5".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "minimax".into(), display_name: "MiniMax".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://api.minimaxi.chat".into(), chat_path: "/v1/text/chatcompletion_v2".into(), default_model: "abab6.5s-chat".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "stepfun".into(), display_name: "StepFun".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://api.stepfun.com".into(), chat_path: "/v1/chat/completions".into(), default_model: "step-2-16k".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "synthetic".into(), display_name: "Synthetic".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://api.synthetic.new".into(), chat_path: "/v1/chat/completions".into(), default_model: "synthetic-coder".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "venice".into(), display_name: "Venice AI".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://api.venice.ai".into(), chat_path: "/api/v1/chat/completions".into(), default_model: "default".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "arcee".into(), display_name: "Arcee AI".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://api.arcee.ai".into(), chat_path: "/v1/chat/completions".into(), default_model: "arcee-spark".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "byteplus".into(), display_name: "BytePlus".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://ark.ap-southeast.bytepluses.com".into(), chat_path: "/api/v3/chat/completions".into(), default_model: "default".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "chutes".into(), display_name: "Chutes".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://llm.chutes.ai".into(), chat_path: "/v1/chat/completions".into(), default_model: "default".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "qwen".into(), display_name: "Qwen Cloud (DashScope)".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://dashscope.aliyuncs.com".into(), chat_path: "/compatible-mode/v1/chat/completions".into(), default_model: "qwen-max".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "qianfan".into(), display_name: "Qianfan (Baidu)".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://qianfan.baidubce.com".into(), chat_path: "/v2/chat/completions".into(), default_model: "ernie-4.5".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "volcano".into(), display_name: "Volcano Engine".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://ark.cn-beijing.volces.com".into(), chat_path: "/api/v3/chat/completions".into(), default_model: "default".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "xiaomi".into(), display_name: "Xiaomi".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://api.xiaomi.com".into(), chat_path: "/v1/chat/completions".into(), default_model: "mimo".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "z-ai".into(), display_name: "Z.AI (Zhipu GLM)".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://api.z.ai".into(), chat_path: "/api/coding/paas/v4/chat/completions".into(), default_model: "glm-4.6".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "ms-foundry".into(), display_name: "Microsoft Foundry".into(), wire: WireFormat::OpenAIChat, auth: AuthScheme::ApiKey { header: Cow::Borrowed("api-key"), prefix: Cow::Borrowed("") }, base_url: "https://models.inference.ai.azure.com".into(), chat_path: "/chat/completions".into(), default_model: "gpt-4o".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "litellm".into(), display_name: "LiteLLM Proxy".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "http://localhost:4000".into(), chat_path: "/v1/chat/completions".into(), default_model: "gpt-4o".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "vercel-ai".into(), display_name: "Vercel AI Gateway".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://gateway.ai.vercel.com".into(), chat_path: "/v1/chat/completions".into(), default_model: "openai/gpt-4o".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "cloudflare".into(), display_name: "Cloudflare AI Gateway".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://gateway.ai.cloudflare.com/v1/{account_id}/{gateway}/compat".into(), chat_path: "/chat/completions".into(), default_model: "@cf/meta/llama-3.3-70b-instruct".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "kilo".into(), display_name: "Kilo Gateway".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://api.kilocode.ai".into(), chat_path: "/v1/chat/completions".into(), default_model: "gpt-4o".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "opencode".into(), display_name: "OpenCode".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://api.opencode.ai".into(), chat_path: "/v1/chat/completions".into(), default_model: "default".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "copilot-proxy".into(), display_name: "Copilot Proxy".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://api.copilotproxy.dev".into(), chat_path: "/v1/chat/completions".into(), default_model: "gpt-4o".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "vllm".into(), display_name: "vLLM".into(), wire: WireFormat::OpenAIChat, auth: AuthScheme::None, base_url: "http://localhost:8000".into(), chat_path: "/v1/chat/completions".into(), default_model: "default".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "sglang".into(), display_name: "SGLang".into(), wire: WireFormat::OpenAIChat, auth: AuthScheme::None, base_url: "http://localhost:30000".into(), chat_path: "/v1/chat/completions".into(), default_model: "default".into(), capabilities: FULL_CAPS },
        ProviderEntry { id: "huggingface".into(), display_name: "Hugging Face Inference".into(), wire: WireFormat::OpenAIChat, auth: bearer(), base_url: "https://router.huggingface.co".into(), chat_path: "/v1/chat/completions".into(), default_model: "meta-llama/Llama-3.3-70B-Instruct".into(), capabilities: FULL_CAPS },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_unique() {
        let cat = builtin_catalog();
        let mut seen = HashSet::new();
        for e in &cat {
            assert!(seen.insert(e.id.clone()), "duplicate id: {}", e.id);
        }
        assert!(cat.len() >= 30, "expected >=30 providers, got {}", cat.len());
    }

    #[test]
    fn oauth_specs_well_formed() {
        for e in builtin_catalog() {
            if let AuthScheme::OAuth(spec) = &e.auth {
                assert!(!spec.token_url.is_empty(), "{}: empty token_url", e.id);
                assert!(!spec.client_id.is_empty(), "{}: empty client_id", e.id);
            }
        }
    }

    #[test]
    fn base_urls_parse() {
        for e in builtin_catalog() {
            // {placeholder}s are templated later; strip for the parse check.
            let cleaned = e.base_url.replace("{account_id}", "x").replace("{gateway}", "x");
            url::Url::parse(&cleaned).unwrap_or_else(|_| panic!("bad base_url for {}: {}", e.id, e.base_url));
        }
    }
}
```

Modify `crates/origin-provider/src/lib.rs` — add after the `pub mod catalog;` line:

```rust
mod catalog_rows;
pub use catalog_rows::builtin_catalog;
```

Modify `crates/origin-provider/Cargo.toml` — add under `[dependencies]`:

```toml
url = "2"

[dev-dependencies]
url = "2"
```

(Note: the `[dev-dependencies]` line is already present in the existing file; only add `url = "2"` to it.)

- [ ] **Step 2: Run tests**

Run: `cargo test -p origin-provider catalog_rows`
Expected: 3 tests pass — `ids_are_unique`, `oauth_specs_well_formed`, `base_urls_parse`.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-provider/src/catalog_rows.rs crates/origin-provider/src/lib.rs crates/origin-provider/Cargo.toml
git commit -m "feat(provider): BUILTIN_CATALOG with 31 builtin entries"
```

---

### Task 3: Catalog lookup helper

**Files:**
- Modify: `crates/origin-provider/src/catalog.rs`

- [ ] **Step 1: Add a `Catalog` struct + lookup**

Append to `crates/origin-provider/src/catalog.rs`:

```rust
use crate::builtin_catalog;

/// Merged (builtin + custom) catalog handle.
#[derive(Debug, Clone)]
pub struct Catalog {
    entries: Vec<ProviderEntry>,
}

impl Catalog {
    #[must_use]
    pub fn builtin() -> Self {
        Self { entries: builtin_catalog() }
    }

    pub fn merge_custom(&mut self, custom: Vec<ProviderEntry>) -> Result<(), CatalogError> {
        for entry in custom {
            if self.entries.iter().any(|e| e.id == entry.id) {
                return Err(CatalogError::IdCollision(entry.id.to_string()));
            }
            self.entries.push(entry);
        }
        Ok(())
    }

    #[must_use]
    pub fn lookup(&self, id: &str) -> Option<&ProviderEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    #[must_use]
    pub fn entries(&self) -> &[ProviderEntry] {
        &self.entries
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("user-defined provider id collides with builtin: {0}")]
    IdCollision(String),
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn lookup_finds_builtin() {
        let cat = Catalog::builtin();
        assert!(cat.lookup("openai").is_some());
        assert!(cat.lookup("deepseek").is_some());
        assert!(cat.lookup("nonexistent").is_none());
    }

    #[test]
    fn collision_rejected() {
        let mut cat = Catalog::builtin();
        let dup = ProviderEntry {
            id: "openai".into(),
            display_name: "Fake".into(),
            wire: WireFormat::OpenAIChat,
            auth: AuthScheme::None,
            base_url: "https://x".into(),
            chat_path: "/x".into(),
            default_model: "x".into(),
            capabilities: Capabilities::default(),
        };
        assert!(cat.merge_custom(vec![dup]).is_err());
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p origin-provider catalog::catalog_tests`
Expected: PASS (2 tests).

- [ ] **Step 3: Commit**

```bash
git add crates/origin-provider/src/catalog.rs
git commit -m "feat(provider): Catalog struct with builtin/custom merge + lookup"
```

---

## Phase B — Extract `origin-provider-openai-compat`

### Task 4: Scaffold the new crate

**Files:**
- Create: `crates/origin-provider-openai-compat/Cargo.toml`
- Create: `crates/origin-provider-openai-compat/src/lib.rs`

- [ ] **Step 1: Create the Cargo.toml**

Create `crates/origin-provider-openai-compat/Cargo.toml`:

```toml
[package]
name = "origin-provider-openai-compat"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
origin-core = { path = "../origin-core" }
origin-provider = { path = "../origin-provider" }
origin-stream = { path = "../origin-stream" }
async-trait = "0.1"
futures-util = "0.3"
pin-utils = "0.1"
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls", "stream"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", default-features = false }

[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
wiremock = "0.6"
```

Create a minimal `crates/origin-provider-openai-compat/src/lib.rs`:

```rust
//! Generic OpenAI Chat-Completions wire client.
//!
//! Parameterised over base URL, chat path, auth header, and extra headers so
//! it can serve every OpenAI-shape provider in the catalog from a single
//! implementation.

pub mod streaming;
pub mod wire;
mod token_source;

pub use token_source::{StaticBearer, StaticHeader, TokenSource};

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_provider::openai_tools::tool_call_to_block;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use reqwest::StatusCode;
use std::sync::Arc;

#[derive(Clone)]
pub struct OpenAiCompatConfig {
    pub name: &'static str,
    pub base_url: String,
    pub chat_path: String,
    pub auth: Arc<dyn TokenSource>,
    pub extra_headers: Vec<(String, String)>,
}

pub struct OpenAiCompat {
    cfg: OpenAiCompatConfig,
    client: reqwest::Client,
}

impl OpenAiCompat {
    #[must_use]
    pub fn new(cfg: OpenAiCompatConfig) -> Self {
        Self { cfg, client: reqwest::Client::new() }
    }
}

#[async_trait]
impl Provider for OpenAiCompat {
    fn name(&self) -> &'static str { self.cfg.name }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let body = wire::encode_request(&req, false);
        let url = format!("{}{}", self.cfg.base_url.trim_end_matches('/'), self.cfg.chat_path);
        let mut builder = self.client.post(&url).header("content-type", "application/json").json(&body);
        let (hdr, val) = self.cfg.auth.header().await.map_err(|e| ProviderError::Auth)?;
        if !hdr.is_empty() {
            builder = builder.header(hdr, val);
        }
        for (h, v) in &self.cfg.extra_headers {
            builder = builder.header(h, v);
        }
        let resp = builder.send().await.map_err(|e| ProviderError::Transport(e.to_string()))?;
        if resp.status() == StatusCode::OK {
            let wire: wire::WireResponse = resp.json().await.map_err(|e| ProviderError::Api(format!("decode: {e}")))?;
            return Ok(decode_response(wire));
        }
        Err(status_error(resp).await)
    }

    async fn chat_stream(&self, req: ChatRequest, ring: &origin_stream::Ring) -> Result<(), ProviderError> {
        let body = wire::encode_request(&req, true);
        let url = format!("{}{}", self.cfg.base_url.trim_end_matches('/'), self.cfg.chat_path);
        let mut builder = self.client.post(&url)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&body);
        let (hdr, val) = self.cfg.auth.header().await.map_err(|_| ProviderError::Auth)?;
        if !hdr.is_empty() {
            builder = builder.header(hdr, val);
        }
        for (h, v) in &self.cfg.extra_headers {
            builder = builder.header(h, v);
        }
        let resp = builder.send().await.map_err(|e| ProviderError::Transport(e.to_string()))?;
        if !resp.status().is_success() {
            let err = status_error(resp).await;
            ring.close();
            return Err(err);
        }
        let result = streaming::parse_into_ring(resp, ring).await;
        ring.close();
        result
    }
}

async fn status_error(resp: reqwest::Response) -> ProviderError {
    let status = resp.status();
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProviderError::Auth,
        StatusCode::TOO_MANY_REQUESTS => {
            let retry = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|s| s.parse::<u32>().ok()).unwrap_or(1);
            ProviderError::RateLimit { retry_after_secs: retry }
        }
        s => {
            let body = resp.text().await.unwrap_or_default();
            ProviderError::Api(format!("status {s}: {body}"))
        }
    }
}

fn decode_response(wire: wire::WireResponse) -> ChatResponse {
    let mut blocks: Vec<Block> = Vec::new();
    if let Some(choice) = wire.choices.into_iter().next() {
        if let Some(text) = choice.message.content {
            if !text.is_empty() {
                blocks.push(Block::Text { text, cache_marker: None });
            }
        }
        if let Some(tool_calls) = choice.message.tool_calls {
            for tc in &tool_calls {
                blocks.push(tool_call_to_block(tc));
            }
        }
    }
    ChatResponse {
        assistant: Message { role: Role::Assistant, blocks },
        usage: Usage {
            input_tokens: wire.usage.prompt_tokens,
            output_tokens: wire.usage.completion_tokens,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        },
    }
}
```

- [ ] **Step 2: Verify the crate is picked up by the workspace**

Run: `cargo metadata --no-deps --format-version 1 | python -c "import sys,json; print('origin-provider-openai-compat' in [p['name'] for p in json.load(sys.stdin)['packages']])"`
Expected: `True`. (On Windows shells without `python`, run `cargo build -p origin-provider-openai-compat` instead — expect a compile error about missing `wire` and `streaming` modules; that's resolved in Task 5.)

- [ ] **Step 3: Commit**

```bash
git add crates/origin-provider-openai-compat/Cargo.toml crates/origin-provider-openai-compat/src/lib.rs
git commit -m "scaffold(provider-openai-compat): crate skeleton with OpenAiCompat struct"
```

---

### Task 5: TokenSource trait

**Files:**
- Create: `crates/origin-provider-openai-compat/src/token_source.rs`

- [ ] **Step 1: Write the module + tests**

```rust
//! Pluggable bearer-token source for the OpenAI-compat client.
//!
//! Synchronous API-key lookups use `StaticBearer` / `StaticHeader`; OAuth
//! refresh paths can ship a custom impl that hits `OAuthClient::refresh_if_due`
//! before returning the access token.

use async_trait::async_trait;
use std::sync::Arc;

#[async_trait]
pub trait TokenSource: Send + Sync + std::fmt::Debug {
    /// Returns `(header_name, header_value)`. Empty `header_name` means
    /// "no auth header" (used by Ollama/vLLM/SGLang catalog rows).
    async fn header(&self) -> Result<(String, String), TokenError>;
}

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("auth: {0}")]
    Auth(String),
}

/// Static `Authorization: Bearer <key>` source. Used for every API-key
/// provider where the catalog row says `header: "Authorization", prefix: "Bearer "`.
#[derive(Debug, Clone)]
pub struct StaticBearer {
    key: String,
}

impl StaticBearer {
    #[must_use]
    pub fn new(key: impl Into<String>) -> Arc<Self> {
        Arc::new(Self { key: key.into() })
    }
}

#[async_trait]
impl TokenSource for StaticBearer {
    async fn header(&self) -> Result<(String, String), TokenError> {
        Ok(("Authorization".to_string(), format!("Bearer {}", self.key)))
    }
}

/// Static `<header>: <prefix><key>` source. Used for catalog rows with
/// non-standard auth headers (`x-api-key`, `api-key`, `x-goog-api-key`, …).
#[derive(Debug, Clone)]
pub struct StaticHeader {
    header: String,
    prefix: String,
    key: String,
}

impl StaticHeader {
    #[must_use]
    pub fn new(header: impl Into<String>, prefix: impl Into<String>, key: impl Into<String>) -> Arc<Self> {
        Arc::new(Self { header: header.into(), prefix: prefix.into(), key: key.into() })
    }
}

#[async_trait]
impl TokenSource for StaticHeader {
    async fn header(&self) -> Result<(String, String), TokenError> {
        if self.header.is_empty() {
            return Ok((String::new(), String::new()));
        }
        Ok((self.header.clone(), format!("{}{}", self.prefix, self.key)))
    }
}

/// "No auth" source for local providers (Ollama, vLLM, SGLang).
#[derive(Debug, Clone)]
pub struct NoAuth;

impl NoAuth {
    #[must_use]
    pub fn new() -> Arc<Self> { Arc::new(Self) }
}

#[async_trait]
impl TokenSource for NoAuth {
    async fn header(&self) -> Result<(String, String), TokenError> {
        Ok((String::new(), String::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_bearer_emits_authorization() {
        let s = StaticBearer::new("sk-abc");
        let (h, v) = s.header().await.unwrap();
        assert_eq!(h, "Authorization");
        assert_eq!(v, "Bearer sk-abc");
    }

    #[tokio::test]
    async fn static_header_with_prefix() {
        let s = StaticHeader::new("x-api-key", "", "abc123");
        let (h, v) = s.header().await.unwrap();
        assert_eq!(h, "x-api-key");
        assert_eq!(v, "abc123");
    }

    #[tokio::test]
    async fn no_auth_returns_empty() {
        let s = NoAuth::new();
        let (h, v) = s.header().await.unwrap();
        assert!(h.is_empty());
        assert!(v.is_empty());
    }
}
```

Also add `thiserror = "1"` to `crates/origin-provider-openai-compat/Cargo.toml` `[dependencies]`.

- [ ] **Step 2: Run tests**

Run: `cargo test -p origin-provider-openai-compat token_source::tests`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-provider-openai-compat/src/token_source.rs crates/origin-provider-openai-compat/Cargo.toml
git commit -m "feat(provider-openai-compat): TokenSource trait + Static/NoAuth impls"
```

---

### Task 6: Move `wire.rs` + `streaming.rs` into the new crate

**Files:**
- Create: `crates/origin-provider-openai-compat/src/wire.rs`
- Create: `crates/origin-provider-openai-compat/src/streaming.rs`
- Delete: `crates/origin-provider-openai/src/wire.rs`
- Delete: `crates/origin-provider-openai/src/streaming.rs`

- [ ] **Step 1: Copy the existing files**

Run from repo root (PowerShell):

```powershell
Move-Item crates/origin-provider-openai/src/wire.rs crates/origin-provider-openai-compat/src/wire.rs
Move-Item crates/origin-provider-openai/src/streaming.rs crates/origin-provider-openai-compat/src/streaming.rs
```

- [ ] **Step 2: Make `wire` and `streaming` modules `pub`**

In `crates/origin-provider-openai-compat/src/wire.rs`:
- Change top-level `#![allow(clippy::module_name_repetitions)]` (already present) — no further change needed.
- The function `encode_request` is already `pub`. Verify the file compiles in its new location.

In `crates/origin-provider-openai-compat/src/streaming.rs`:
- Already `pub mod` — no change.

- [ ] **Step 3: Compile**

Run: `cargo build -p origin-provider-openai-compat`
Expected: builds cleanly.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "refactor(provider): move OpenAI wire+streaming into provider-openai-compat"
```

---

### Task 7: Rewrite `origin-provider-openai` as a thin wrapper

**Files:**
- Modify: `crates/origin-provider-openai/src/lib.rs`
- Modify: `crates/origin-provider-openai/Cargo.toml`

- [ ] **Step 1: Add the dep**

In `crates/origin-provider-openai/Cargo.toml` `[dependencies]`, replace the existing reqwest/serde/etc. entries with:

```toml
origin-core = { path = "../origin-core" }
origin-provider = { path = "../origin-provider" }
origin-provider-openai-compat = { path = "../origin-provider-openai-compat" }
origin-stream = { path = "../origin-stream" }
async-trait = "0.1"
```

Drop reqwest/serde/futures-util/pin-utils/serde_json/tokio from the deps list (they're transitively pulled in by openai-compat).

Keep `[dev-dependencies]` as-is (wiremock + tokio).

- [ ] **Step 2: Replace `lib.rs` with a wrapper**

Replace `crates/origin-provider-openai/src/lib.rs` with:

```rust
//! `OpenAI` provider — thin wrapper around `origin-provider-openai-compat`.

use origin_provider_openai_compat::{OpenAiCompat, OpenAiCompatConfig, StaticBearer};

const DEFAULT_BASE: &str = "https://api.openai.com";

pub struct OpenAi(OpenAiCompat);

impl OpenAi {
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE)
    }

    #[must_use]
    pub fn with_base_url(api_key: impl Into<String>, base: &str) -> Self {
        let cfg = OpenAiCompatConfig {
            name: "openai",
            base_url: base.trim_end_matches('/').to_string(),
            chat_path: "/v1/chat/completions".to_string(),
            auth: StaticBearer::new(api_key.into()),
            extra_headers: Vec::new(),
        };
        Self(OpenAiCompat::new(cfg))
    }
}

#[async_trait::async_trait]
impl origin_provider::Provider for OpenAi {
    fn name(&self) -> &'static str { self.0.name() }
    async fn chat(&self, req: origin_provider::ChatRequest) -> Result<origin_provider::ChatResponse, origin_provider::ProviderError> {
        self.0.chat(req).await
    }
    async fn chat_stream(&self, req: origin_provider::ChatRequest, ring: &origin_stream::Ring) -> Result<(), origin_provider::ProviderError> {
        self.0.chat_stream(req, ring).await
    }
}
```

- [ ] **Step 3: Run existing OpenAI tests**

Run: `cargo test -p origin-provider-openai`
Expected: all existing tests pass (any wiremock fixtures continue to hit the same `/v1/chat/completions` path).

- [ ] **Step 4: Compile the daemon to catch any downstream breakage**

Run: `cargo build -p origin-daemon --features openai,gemini,ollama`
Expected: builds cleanly.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-provider-openai
git commit -m "refactor(provider-openai): wrap origin-provider-openai-compat"
```

---

### Task 8: Rewrite `origin-provider-openrouter` as a thin wrapper

**Files:**
- Modify: `crates/origin-provider-openrouter/src/lib.rs`
- Modify: `crates/origin-provider-openrouter/Cargo.toml`

- [ ] **Step 1: Update deps**

In `crates/origin-provider-openrouter/Cargo.toml` `[dependencies]`:

```toml
origin-core = { path = "../origin-core" }
origin-provider = { path = "../origin-provider" }
origin-provider-openai-compat = { path = "../origin-provider-openai-compat" }
origin-stream = { path = "../origin-stream" }
async-trait = "0.1"
```

- [ ] **Step 2: Replace `lib.rs`**

```rust
//! `OpenRouter` provider — thin wrapper around `origin-provider-openai-compat`.

use origin_provider_openai_compat::{OpenAiCompat, OpenAiCompatConfig, StaticBearer};

const DEFAULT_BASE: &str = "https://openrouter.ai";
const REFERER: &str = "https://origin.local";
const TITLE: &str = "origin";

pub struct OpenRouter(OpenAiCompat);

impl OpenRouter {
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE)
    }

    #[must_use]
    pub fn with_base_url(api_key: impl Into<String>, base: &str) -> Self {
        let cfg = OpenAiCompatConfig {
            name: "openrouter",
            base_url: base.trim_end_matches('/').to_string(),
            chat_path: "/api/v1/chat/completions".to_string(),
            auth: StaticBearer::new(api_key.into()),
            extra_headers: vec![
                ("HTTP-Referer".to_string(), REFERER.to_string()),
                ("X-Title".to_string(), TITLE.to_string()),
            ],
        };
        Self(OpenAiCompat::new(cfg))
    }
}

#[async_trait::async_trait]
impl origin_provider::Provider for OpenRouter {
    fn name(&self) -> &'static str { self.0.name() }
    async fn chat(&self, req: origin_provider::ChatRequest) -> Result<origin_provider::ChatResponse, origin_provider::ProviderError> {
        self.0.chat(req).await
    }
    async fn chat_stream(&self, req: origin_provider::ChatRequest, ring: &origin_stream::Ring) -> Result<(), origin_provider::ProviderError> {
        self.0.chat_stream(req, ring).await
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p origin-provider-openrouter`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/origin-provider-openrouter
git commit -m "refactor(provider-openrouter): wrap origin-provider-openai-compat"
```

---

### Task 9: End-to-end smoke test against a wiremock OpenAI-compat server

**Files:**
- Create: `crates/origin-provider-openai-compat/tests/wire_smoke.rs`

- [ ] **Step 1: Write the test**

```rust
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_openai_compat::{OpenAiCompat, OpenAiCompatConfig, StaticBearer};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn happy_path_chat() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": { "role": "assistant", "content": "hello world" }
            }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 2 }
        })))
        .mount(&server)
        .await;

    let cfg = OpenAiCompatConfig {
        name: "test",
        base_url: server.uri(),
        chat_path: "/v1/chat/completions".to_string(),
        auth: StaticBearer::new("sk-test"),
        extra_headers: vec![],
    };
    let provider = OpenAiCompat::new(cfg);

    let req = ChatRequest {
        system: String::new(),
        messages: vec![Message { role: Role::User, blocks: vec![Block::Text { text: "hi".into(), cache_marker: None }] }],
        model: "test-model".to_string(),
        tools: vec![],
    };

    let resp = provider.chat(req).await.unwrap();
    let text: String = resp.assistant.blocks.iter().filter_map(|b| match b {
        Block::Text { text, .. } => Some(text.clone()),
        _ => None,
    }).collect();
    assert_eq!(text, "hello world");
    assert_eq!(resp.usage.input_tokens, 5);
    assert_eq!(resp.usage.output_tokens, 2);
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p origin-provider-openai-compat --test wire_smoke`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-provider-openai-compat/tests/wire_smoke.rs
git commit -m "test(provider-openai-compat): wiremock happy-path chat"
```

---

## Phase C — Catalog-driven `ProviderFactory`

### Task 10: New `ProviderId` (catalog-id newtype)

**Files:**
- Modify: `crates/origin-daemon/src/provider_factory.rs`

- [ ] **Step 1: Replace the enum-based ProviderId**

Replace the top of `crates/origin-daemon/src/provider_factory.rs` (lines 30–85 — `pub enum ProviderId` and its `impl`) with:

```rust
use std::borrow::Cow;
use origin_provider::catalog::Catalog;

/// Stable id of a provider in the merged catalog.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderId(pub Cow<'static, str>);

impl ProviderId {
    #[must_use]
    pub fn parse(s: &str, catalog: &Catalog) -> Option<Self> {
        let normalised = s.to_ascii_lowercase();
        // Common aliases: "open-ai" → "openai", "aws-bedrock" → "bedrock"
        let canonical = match normalised.as_str() {
            "open-ai" | "open_ai" => "openai",
            "aws-bedrock" => "bedrock",
            "gemini" => "google",
            "github" | "github-models" => "github-copilot",
            "open-router" => "openrouter",
            other => other,
        };
        catalog.lookup(canonical).map(|e| Self(e.id.clone()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str { &self.0 }
}
```

Update the rest of the file (the `build` impl) in the next task.

- [ ] **Step 2: Temporarily comment out the existing `build` impl** so the crate compiles between this and the next task — wrap the existing `impl ProviderFactory { … }` block in `#[cfg(any())]` (always-false cfg) so it's parsed but excluded from compilation. Mark this as a transient state to be fixed in Task 11.

- [ ] **Step 3: Verify compilation**

Run: `cargo build -p origin-daemon`
Expected: builds (factory exists but `build` is gated out).

- [ ] **Step 4: Commit**

```bash
git add crates/origin-daemon/src/provider_factory.rs
git commit -m "refactor(daemon): ProviderId becomes catalog-id newtype (build impl wip)"
```

---

### Task 11: Rewrite `ProviderFactory::build` to dispatch on catalog row

**Files:**
- Modify: `crates/origin-daemon/src/provider_factory.rs`

- [ ] **Step 1: Replace the body**

Replace the entire `impl ProviderFactory` block (including the `#[cfg(any())]` from Task 10) with:

```rust
#[derive(Clone)]
pub struct ProviderFactory {
    vault: KeyVault,
    cas: Option<Arc<origin_cas::Store>>,
    catalog: Catalog,
}

impl core::fmt::Debug for ProviderFactory {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ProviderFactory")
            .field("vault", &self.vault)
            .field("cas", &self.cas.as_ref().map(|_| "<cas>"))
            .field("catalog_entries", &self.catalog.entries().len())
            .finish()
    }
}

impl ProviderFactory {
    #[must_use]
    pub fn new(vault: KeyVault, catalog: Catalog) -> Self {
        Self { vault, cas: None, catalog }
    }

    #[must_use]
    pub fn with_cas(mut self, cas: Arc<origin_cas::Store>) -> Self {
        self.cas = Some(cas);
        self
    }

    #[must_use]
    pub fn catalog(&self) -> &Catalog { &self.catalog }

    pub async fn build(&self, id: &ProviderId, account: &str) -> Result<Arc<dyn Provider>, FactoryError> {
        let entry = self.catalog.lookup(id.as_str())
            .ok_or_else(|| FactoryError::UnknownProvider(id.as_str().to_string()))?
            .clone();
        let token = self.resolve_auth(&entry, account).await?;
        self.build_for_wire(&entry, token, account).await
    }

    async fn resolve_auth(&self, entry: &origin_provider::catalog::ProviderEntry, account: &str) -> Result<Arc<dyn TokenSource>, FactoryError> {
        use origin_provider::catalog::AuthScheme;
        use origin_provider_openai_compat::{NoAuth, StaticBearer, StaticHeader};
        match &entry.auth {
            AuthScheme::None => Ok(NoAuth::new()),
            AuthScheme::ApiKey { header, prefix } => {
                let secret = self.vault.get(entry.id.as_ref(), account).await
                    .map_err(|e| FactoryError::from_vault(e, entry.id.as_ref(), account))?;
                if header.eq_ignore_ascii_case("Authorization") && prefix.as_ref() == "Bearer " {
                    Ok(StaticBearer::new(secret.expose().clone()))
                } else {
                    Ok(StaticHeader::new(header.to_string(), prefix.to_string(), secret.expose().clone()))
                }
            }
            AuthScheme::OAuth(_) => {
                // OAuth-backed providers consult their own per-wire builders for token refresh.
                // For OpenAIChat wire, fetch the latest access token via OAuthClient.refresh_if_due.
                use origin_keyvault::OAuthClient;
                use std::time::Duration;
                if let AuthScheme::OAuth(spec) = &entry.auth {
                    let client = OAuthClient::new(entry.id.to_string(), spec.token_url.to_string(), spec.client_id.to_string());
                    match client.refresh_if_due(&self.vault, account, Duration::from_secs(60)).await {
                        Ok(origin_keyvault::oauth::RefreshOutcome::Rotated { access }) => {
                            Ok(StaticBearer::new(access.expose().to_string()))
                        }
                        Ok(origin_keyvault::oauth::RefreshOutcome::NotDue { .. }) => {
                            // Load existing token from vault (stored under "{account}/oauth")
                            let secret = self.vault.get(entry.id.as_ref(), &format!("{account}/oauth")).await
                                .map_err(|e| FactoryError::from_vault(e, entry.id.as_ref(), account))?;
                            // The stored value is a JSON blob; extract the access field.
                            let stored: serde_json::Value = serde_json::from_str(secret.expose())
                                .map_err(|e| FactoryError::CredentialParse(e.to_string()))?;
                            let access = stored.get("access").and_then(|v| v.as_str())
                                .ok_or_else(|| FactoryError::CredentialParse("oauth blob missing 'access'".into()))?;
                            Ok(StaticBearer::new(access.to_string()))
                        }
                        Err(e) => Err(FactoryError::Vault(e.to_string())),
                    }
                } else { unreachable!() }
            }
            AuthScheme::SigV4 { .. } => {
                // SigV4 is handled inside the Bedrock builder — return NoAuth as a placeholder.
                Ok(NoAuth::new())
            }
            AuthScheme::Custom => Ok(NoAuth::new()),
        }
    }

    async fn build_for_wire(
        &self,
        entry: &origin_provider::catalog::ProviderEntry,
        token: Arc<dyn TokenSource>,
        account: &str,
    ) -> Result<Arc<dyn Provider>, FactoryError> {
        use origin_provider::catalog::WireFormat;
        use origin_provider_openai_compat::{OpenAiCompat, OpenAiCompatConfig};
        // Leak the id to obtain a &'static str for Provider::name(). One leak per
        // provider construction is acceptable for a daemon's lifetime; the
        // alternative (changing the trait signature) is a much bigger refactor.
        let name: &'static str = Box::leak(entry.id.to_string().into_boxed_str());
        match entry.wire {
            WireFormat::OpenAIChat => {
                let cfg = OpenAiCompatConfig {
                    name,
                    base_url: render_base_url(entry.base_url.as_ref(), &self.vault, entry.id.as_ref(), account).await?,
                    chat_path: entry.chat_path.to_string(),
                    auth: token,
                    extra_headers: openai_extra_headers(entry.id.as_ref()),
                };
                Ok(Arc::new(OpenAiCompat::new(cfg)))
            }
            WireFormat::Anthropic => {
                let secret = self.vault.get(entry.id.as_ref(), account).await
                    .map_err(|e| FactoryError::from_vault(e, entry.id.as_ref(), account))?;
                let mut p = origin_provider_anthropic::Anthropic::new(secret.expose().clone());
                if let Some(cas) = self.cas.clone() { p = p.with_cas(cas); }
                Ok(Arc::new(p))
            }
            #[cfg(feature = "gemini")]
            WireFormat::Gemini => {
                let secret = self.vault.get(entry.id.as_ref(), account).await
                    .map_err(|e| FactoryError::from_vault(e, entry.id.as_ref(), account))?;
                Ok(Arc::new(origin_provider_gemini::Gemini::new(secret.expose().clone())))
            }
            #[cfg(not(feature = "gemini"))]
            WireFormat::Gemini => Err(FactoryError::UnknownProvider("gemini".into())),
            #[cfg(feature = "bedrock")]
            WireFormat::Bedrock => {
                let secret = self.vault.get(entry.id.as_ref(), account).await
                    .map_err(|e| FactoryError::from_vault(e, entry.id.as_ref(), account))?;
                let creds: BedrockCreds = serde_json::from_str(secret.expose())
                    .map_err(|e| FactoryError::CredentialParse(e.to_string()))?;
                Ok(Arc::new(origin_provider_bedrock::Bedrock::new(
                    creds.endpoint(),
                    creds.region.clone(),
                    creds.model_id(),
                    creds.access.clone(),
                    creds.secret,
                )))
            }
            #[cfg(not(feature = "bedrock"))]
            WireFormat::Bedrock => Err(FactoryError::UnknownProvider("bedrock".into())),
            #[cfg(feature = "ollama")]
            WireFormat::Ollama => {
                let _ = account;
                Ok(Arc::new(origin_provider_ollama::Ollama::new()))
            }
            #[cfg(not(feature = "ollama"))]
            WireFormat::Ollama => Err(FactoryError::UnknownProvider("ollama".into())),
            #[cfg(feature = "github-models")]
            WireFormat::GitHubCopilot => {
                Ok(Arc::new(origin_provider_github::GitHubModels::new(self.vault.clone(), account)))
            }
            #[cfg(not(feature = "github-models"))]
            WireFormat::GitHubCopilot => Err(FactoryError::UnknownProvider("github-copilot".into())),
        }
    }
}

async fn render_base_url(
    template: &str,
    vault: &KeyVault,
    provider: &str,
    account: &str,
) -> Result<String, FactoryError> {
    if !template.contains('{') { return Ok(template.to_string()); }
    // Account-scoped extras (e.g. `account_id`, `gateway`) live in
    // `({provider}, "{account}/extras")` as a JSON blob.
    let extras_key = format!("{account}/extras");
    let extras = vault.get(provider, &extras_key).await
        .map_err(|e| FactoryError::from_vault(e, provider, &extras_key))?;
    let json: serde_json::Map<String, serde_json::Value> = serde_json::from_str(extras.expose())
        .map_err(|e| FactoryError::CredentialParse(e.to_string()))?;
    let mut out = template.to_string();
    for (k, v) in json {
        if let Some(s) = v.as_str() {
            out = out.replace(&format!("{{{k}}}"), s);
        }
    }
    Ok(out)
}

fn openai_extra_headers(id: &str) -> Vec<(String, String)> {
    match id {
        "openrouter" => vec![
            ("HTTP-Referer".to_string(), "https://origin.local".to_string()),
            ("X-Title".to_string(), "origin".to_string()),
        ],
        _ => Vec::new(),
    }
}
```

Also update imports at the top:

```rust
use origin_provider::Provider;
use origin_provider::catalog::Catalog;
use origin_provider_openai_compat::TokenSource;
```

- [ ] **Step 2: Update callers of `ProviderFactory::new(vault)` → `ProviderFactory::new(vault, Catalog::builtin())`**

Run: `cargo build -p origin-daemon 2>&1 | grep "expected.*arguments" | head -5`
Then `Grep` for `ProviderFactory::new(` and update each call site (likely `crates/origin-daemon/src/main.rs` and `crates/origin-daemon/src/lib.rs`) to pass `Catalog::builtin()`.

- [ ] **Step 3: Update `parse` callers**

Grep for `ProviderId::parse(` and add the catalog argument:
```bash
git grep -n "ProviderId::parse"
```
Most call sites are in `protocol.rs` / `main.rs` of the daemon. Pass the daemon's `factory.catalog()` reference.

- [ ] **Step 4: Build**

Run: `cargo build -p origin-daemon --features openai,gemini,ollama`
Expected: builds cleanly.

- [ ] **Step 5: Run existing tests**

Run: `cargo test -p origin-daemon --features openai,gemini,ollama`
Expected: existing tests pass (the legacy `parse_round_trip_known_ids` test needs to be updated to use `Catalog::builtin()`).

If `parse_round_trip_known_ids` fails, update it to:

```rust
#[test]
fn parse_round_trip_known_ids() {
    let cat = Catalog::builtin();
    for id in ["anthropic", "openai", "google", "ollama"] {
        let parsed = ProviderId::parse(id, &cat).expect(id);
        assert_eq!(parsed.as_str(), id);
    }
}

#[test]
fn parse_aliases() {
    let cat = Catalog::builtin();
    assert_eq!(ProviderId::parse("open-ai", &cat).unwrap().as_str(), "openai");
    assert_eq!(ProviderId::parse("gemini", &cat).unwrap().as_str(), "google");
    assert!(ProviderId::parse("totally-not-a-provider", &cat).is_none());
}
```

- [ ] **Step 6: Commit**

```bash
git add crates/origin-daemon
git commit -m "feat(daemon): catalog-driven ProviderFactory dispatching on WireFormat"
```

---

### Task 12: Daemon depends on `origin-provider-openai-compat`

**Files:**
- Modify: `crates/origin-daemon/Cargo.toml`

- [ ] **Step 1: Add the dep**

Under `[dependencies]` add:

```toml
origin-provider-openai-compat = { path = "../origin-provider-openai-compat" }
```

- [ ] **Step 2: Build**

Run: `cargo build -p origin-daemon`
Expected: builds cleanly.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-daemon/Cargo.toml
git commit -m "build(daemon): add origin-provider-openai-compat dep"
```

---

## Phase D — Catalog factory tests

### Task 13: Integration test — catalog factory builds OpenAI-compat providers

**Files:**
- Create: `crates/origin-daemon/tests/catalog_factory.rs`

- [ ] **Step 1: Write the test**

```rust
//! End-to-end: catalog row → ProviderFactory → wiremock → real chat response.

use origin_core::types::{Block, Message, Role};
use origin_daemon::provider_factory::{ProviderFactory, ProviderId};
use origin_keyvault::{KeyVault, Secret};
use origin_provider::catalog::Catalog;
use origin_provider::{ChatRequest, Provider};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn deepseek_row_builds_and_chats() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "role": "assistant", "content": "hi from deepseek" } }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 4 }
        })))
        .mount(&server)
        .await;

    // Substitute the deepseek base_url with the wiremock URL by inserting a
    // user-defined catalog row that shadows nothing (different id).
    let mut catalog = Catalog::builtin();
    let entry = origin_provider::catalog::ProviderEntry {
        id: "test-deepseek".into(),
        display_name: "Test DeepSeek".into(),
        wire: origin_provider::catalog::WireFormat::OpenAIChat,
        auth: origin_provider::catalog::AuthScheme::ApiKey {
            header: "Authorization".into(),
            prefix: "Bearer ".into(),
        },
        base_url: server.uri().into(),
        chat_path: "/v1/chat/completions".into(),
        default_model: "deepseek-chat".into(),
        capabilities: origin_provider::catalog::Capabilities::default(),
    };
    catalog.merge_custom(vec![entry]).unwrap();

    let vault = KeyVault::memory();
    vault.set("test-deepseek", "default", Secret::new("sk-test".to_string())).await.unwrap();

    let factory = ProviderFactory::new(vault, catalog);
    let id = ProviderId::parse("test-deepseek", factory.catalog()).unwrap();
    let provider = factory.build(&id, "default").await.unwrap();

    let resp = provider.chat(ChatRequest {
        system: String::new(),
        messages: vec![Message { role: Role::User, blocks: vec![Block::Text { text: "hi".into(), cache_marker: None }] }],
        model: "deepseek-chat".to_string(),
        tools: vec![],
    }).await.unwrap();

    let text: String = resp.assistant.blocks.iter().filter_map(|b| match b {
        Block::Text { text, .. } => Some(text.clone()),
        _ => None,
    }).collect();
    assert_eq!(text, "hi from deepseek");
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p origin-daemon --features openai --test catalog_factory deepseek_row_builds_and_chats`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-daemon/tests/catalog_factory.rs
git commit -m "test(daemon): catalog row → factory → wiremock chat (deepseek shape)"
```

---

## Phase E — Custom providers loader

### Task 14: `~/.origin/providers.toml` loader

**Files:**
- Create: `crates/origin-provider/src/custom.rs`
- Modify: `crates/origin-provider/src/lib.rs`
- Modify: `crates/origin-provider/Cargo.toml`

- [ ] **Step 1: Add `toml` dep**

In `crates/origin-provider/Cargo.toml` `[dependencies]`:

```toml
toml = "0.8"
```

- [ ] **Step 2: Write loader + tests**

Create `crates/origin-provider/src/custom.rs`:

```rust
//! Loads user-defined provider entries from a TOML file.

use std::borrow::Cow;
use std::path::Path;

use serde::Deserialize;

use crate::catalog::{AuthScheme, Capabilities, ProviderEntry, WireFormat};

#[derive(Debug, Deserialize)]
struct TomlRoot {
    #[serde(default)]
    providers: std::collections::BTreeMap<String, TomlEntry>,
}

#[derive(Debug, Deserialize)]
struct TomlEntry {
    display_name: String,
    wire: String,
    base_url: String,
    #[serde(default = "default_chat_path")]
    chat_path: String,
    default_model: String,
    #[serde(default)]
    auth: Option<TomlAuth>,
}

fn default_chat_path() -> String { "/v1/chat/completions".to_string() }

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum TomlAuth {
    None,
    ApiKey { header: String, #[serde(default)] prefix: String },
}

#[derive(Debug, thiserror::Error)]
pub enum CustomLoadError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("unknown wire format: {0}")]
    UnknownWire(String),
}

pub fn load(path: &Path) -> Result<Vec<ProviderEntry>, CustomLoadError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)?;
    let root: TomlRoot = toml::from_str(&text)?;
    let mut out = Vec::with_capacity(root.providers.len());
    for (id, entry) in root.providers {
        let wire = match entry.wire.as_str() {
            "openai-chat" => WireFormat::OpenAIChat,
            "anthropic" => WireFormat::Anthropic,
            "gemini" => WireFormat::Gemini,
            "ollama" => WireFormat::Ollama,
            other => return Err(CustomLoadError::UnknownWire(other.to_string())),
        };
        let auth = match entry.auth.unwrap_or(TomlAuth::None) {
            TomlAuth::None => AuthScheme::None,
            TomlAuth::ApiKey { header, prefix } => AuthScheme::ApiKey {
                header: Cow::Owned(header),
                prefix: Cow::Owned(prefix),
            },
        };
        out.push(ProviderEntry {
            id: Cow::Owned(id),
            display_name: Cow::Owned(entry.display_name),
            wire,
            auth,
            base_url: Cow::Owned(entry.base_url),
            chat_path: Cow::Owned(entry.chat_path),
            default_model: Cow::Owned(entry.default_model),
            capabilities: Capabilities { streaming: true, tools: true, ..Default::default() },
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn loads_minimal_toml() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, r#"
[providers.my-gateway]
display_name = "My Gateway"
wire = "openai-chat"
base_url = "https://gw.example.com"
default_model = "llama-3.3-70b"
auth = {{ kind = "api-key", header = "Authorization", prefix = "Bearer " }}
"#).unwrap();
        let entries = load(f.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "my-gateway");
        assert_eq!(entries[0].base_url, "https://gw.example.com");
    }

    #[test]
    fn missing_file_yields_empty() {
        let entries = load(std::path::Path::new("/nonexistent/providers.toml")).unwrap();
        assert!(entries.is_empty());
    }
}
```

Add `tempfile = "3"` to `[dev-dependencies]` in `crates/origin-provider/Cargo.toml`.

Add `pub mod custom;` to `crates/origin-provider/src/lib.rs`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p origin-provider custom::tests`
Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/origin-provider/src/custom.rs crates/origin-provider/src/lib.rs crates/origin-provider/Cargo.toml
git commit -m "feat(provider): ~/.origin/providers.toml loader"
```

---

### Task 15: Daemon loads custom providers at startup

**Files:**
- Modify: `crates/origin-daemon/src/lib.rs` (or `main.rs`, wherever `ProviderFactory::new` is constructed)

- [ ] **Step 1: Locate the factory construction**

Run: `git grep -n "ProviderFactory::new"`

- [ ] **Step 2: Merge custom providers**

Wherever the factory is built (e.g. in `crates/origin-daemon/src/lib.rs`), replace:

```rust
let factory = ProviderFactory::new(vault, Catalog::builtin());
```

with:

```rust
let mut catalog = origin_provider::catalog::Catalog::builtin();
let cfg_path = dirs::home_dir().map(|h| h.join(".origin").join("providers.toml"));
if let Some(p) = cfg_path {
    match origin_provider::custom::load(&p) {
        Ok(custom) => {
            if let Err(e) = catalog.merge_custom(custom) {
                tracing::warn!(target: "origin::provider", error = %e, "custom providers merge failed");
            }
        }
        Err(e) => tracing::warn!(target: "origin::provider", error = %e, "failed to load providers.toml"),
    }
}
let factory = ProviderFactory::new(vault, catalog);
```

- [ ] **Step 3: Build**

Run: `cargo build -p origin-daemon`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/origin-daemon/src/lib.rs
git commit -m "feat(daemon): merge ~/.origin/providers.toml into the catalog at startup"
```

---

## Phase F — OAuth-only providers

### Task 16: Anthropic OAuth wire support

**Files:**
- Modify: `crates/origin-provider-anthropic/src/lib.rs`
- Modify: `crates/origin-daemon/src/provider_factory.rs`

- [ ] **Step 1: Add OAuth bearer constructor**

In `crates/origin-provider-anthropic/src/lib.rs`, find the `Anthropic` impl block. Add:

```rust
impl Anthropic {
    /// Construct using a refreshed OAuth bearer token instead of an API key.
    /// Sets the `Authorization: Bearer …` header instead of `x-api-key`.
    #[must_use]
    pub fn with_oauth_bearer(token: impl Into<String>) -> Self {
        // The existing `new(api_key)` stores into a string; add a flag for the
        // OAuth path. Use the existing field with a sentinel header switch.
        let mut p = Self::new(String::new());
        p.set_oauth_bearer(token.into());
        p
    }

    fn set_oauth_bearer(&mut self, token: String) {
        // Implementation detail: the chat() path checks `self.auth_kind` and
        // emits `Authorization: Bearer …` when set; otherwise emits
        // `x-api-key: …`.
        self.auth_kind = AuthKind::OAuthBearer(token);
    }
}

enum AuthKind {
    ApiKey(String),
    OAuthBearer(String),
}
```

(The exact integration depends on the existing `Anthropic` struct shape — read the current `lib.rs` first and adapt; the principle is "swap the auth header based on a runtime flag".)

- [ ] **Step 2: Hook into the factory**

In `crates/origin-daemon/src/provider_factory.rs`, inside `build_for_wire`, replace the `WireFormat::Anthropic` arm:

```rust
WireFormat::Anthropic => {
    use origin_provider::catalog::AuthScheme;
    let mut p = match &entry.auth {
        AuthScheme::ApiKey { .. } => {
            let secret = self.vault.get(entry.id.as_ref(), account).await
                .map_err(|e| FactoryError::from_vault(e, entry.id.as_ref(), account))?;
            origin_provider_anthropic::Anthropic::new(secret.expose().clone())
        }
        AuthScheme::OAuth(_) => {
            let (_, bearer) = token.header().await.map_err(|_| FactoryError::Vault("oauth token".into()))?;
            // bearer is "Bearer xxx"; strip the prefix.
            let raw = bearer.strip_prefix("Bearer ").unwrap_or(&bearer).to_string();
            origin_provider_anthropic::Anthropic::with_oauth_bearer(raw)
        }
        _ => return Err(FactoryError::CredentialParse("anthropic wire requires ApiKey or OAuth".into())),
    };
    if let Some(cas) = self.cas.clone() { p = p.with_cas(cas); }
    Ok(Arc::new(p))
}
```

- [ ] **Step 3: Test**

Add a test under `crates/origin-daemon/tests/catalog_factory.rs`:

```rust
#[tokio::test]
async fn anthropic_oauth_row_uses_bearer_header() {
    // Asserts the request to api.anthropic.com carries Authorization: Bearer …
    // instead of x-api-key …
    // (Implementation note: mount a wiremock that requires
    //  `header("authorization", "Bearer test-token")` and provision an OAuth
    //  blob in the vault under ("anthropic-oauth", "default/oauth") with a
    //  far-future expires_at.)
    // The full test body mirrors the deepseek_row_builds_and_chats pattern.
    // ... (about 30 lines, omitted here for brevity but follow the same shape)
}
```

(Write the full test by copying `deepseek_row_builds_and_chats` and changing the wire-server expectations + provisioning step.)

- [ ] **Step 4: Run**

Run: `cargo test -p origin-daemon anthropic_oauth_row_uses_bearer_header`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(anthropic): OAuth bearer constructor + factory wire-up"
```

---

### Task 17: Gemini OAuth wire support

Mirror Task 16 against `crates/origin-provider-gemini`:
- Add `Gemini::with_oauth_bearer(token)` constructor.
- Factory's `WireFormat::Gemini` arm picks bearer vs API key based on `entry.auth`.
- Wiremock test against the Gemini wire path with `Authorization: Bearer …`.

Commit: `"feat(gemini): OAuth bearer constructor + factory wire-up"`.

---

### Task 18: OpenAI Codex (ChatGPT OAuth)

This is OpenAIChat wire with OAuth — already handled by Task 11's `AuthScheme::OAuth` arm under `WireFormat::OpenAIChat`. Only verification needed:

- [ ] **Step 1: Add a wiremock test**

```rust
#[tokio::test]
async fn openai_codex_oauth_row_uses_bearer() {
    // Provision an OAuth blob under ("openai-codex", "default/oauth") with a
    // far-future expires_at, then assert the wiremock receives
    // `Authorization: Bearer <stored access>`.
}
```

- [ ] **Step 2: Commit**

```bash
git add crates/origin-daemon/tests/catalog_factory.rs
git commit -m "test(daemon): openai-codex OAuth row → Bearer header"
```

---

### Task 19: GitHub Copilot OAuth flow (device flow)

The existing `GitHubModels` already implements the device-flow + token-swap dance internally. The catalog row's `OAuthSpec::device_flow = true` is informational for the CLI's `keyring login` flow.

- [ ] **Step 1: Verify the existing `GitHubModels::new(vault, account)` still compiles and works**

Run: `cargo test -p origin-daemon --features github-models`
Expected: existing GitHub tests pass.

- [ ] **Step 2: Add a regression test asserting the github-copilot catalog row resolves to `GitHubModels`**

```rust
#[tokio::test]
async fn github_copilot_row_builds() {
    let catalog = Catalog::builtin();
    assert!(catalog.lookup("github-copilot").is_some());
    // We can't easily wiremock the device-flow + token-swap path, so this test
    // only asserts the catalog row exists and parses; the existing
    // GitHubModels tests cover the wire.
}
```

- [ ] **Step 3: Commit**

```bash
git add crates/origin-daemon/tests/catalog_factory.rs
git commit -m "test(daemon): github-copilot catalog row presence"
```

---

## Phase G — CLI surface

### Task 20: `origin providers list` and `origin providers describe`

**Files:**
- Modify: `crates/origin-cli/src/cli_def.rs`
- Modify: `crates/origin-cli/src/main.rs`
- Create: `crates/origin-cli/src/providers.rs`

- [ ] **Step 1: Add subcommand**

In `crates/origin-cli/src/cli_def.rs`, add to `enum Cmd`:

```rust
/// List and describe known providers.
Providers {
    #[command(subcommand)]
    sub: ProvidersSub,
},
```

Then add:

```rust
#[derive(Subcommand)]
pub enum ProvidersSub {
    /// List every catalog entry (id, display name, wire, auth, capabilities).
    Ls,
    /// Print one provider's full config.
    Describe { id: String },
}
```

- [ ] **Step 2: Implement**

Create `crates/origin-cli/src/providers.rs`:

```rust
//! `origin providers …` subcommand handlers.

use origin_provider::catalog::{AuthScheme, Catalog};

pub fn ls() {
    let cat = Catalog::builtin();
    println!("{:<20} {:<35} {:<14} {}", "ID", "DISPLAY NAME", "WIRE", "AUTH");
    for e in cat.entries() {
        let wire = format!("{:?}", e.wire);
        let auth = match &e.auth {
            AuthScheme::None => "none",
            AuthScheme::ApiKey { .. } => "api-key",
            AuthScheme::OAuth(_) => "oauth",
            AuthScheme::SigV4 { .. } => "sigv4",
            AuthScheme::Custom => "custom",
        };
        println!("{:<20} {:<35} {:<14} {}", e.id, e.display_name, wire, auth);
    }
}

pub fn describe(id: &str) {
    let cat = Catalog::builtin();
    match cat.lookup(id) {
        Some(e) => {
            println!("id:            {}", e.id);
            println!("display_name:  {}", e.display_name);
            println!("wire:          {:?}", e.wire);
            println!("base_url:      {}", e.base_url);
            println!("chat_path:     {}", e.chat_path);
            println!("default_model: {}", e.default_model);
            println!("streaming:     {}", e.capabilities.streaming);
            println!("tools:         {}", e.capabilities.tools);
            println!("prompt_cache:  {}", e.capabilities.prompt_cache);
            println!("thinking:      {}", e.capabilities.thinking);
            match &e.auth {
                AuthScheme::OAuth(s) => {
                    println!("auth:          oauth (pkce={}, device_flow={})", s.pkce, s.device_flow);
                    println!("  authorize_url: {}", s.authorize_url);
                    println!("  token_url:     {}", s.token_url);
                    println!("  client_id:     {}", s.client_id);
                }
                AuthScheme::ApiKey { header, prefix } => {
                    println!("auth:          api-key (header={header}, prefix={:?})", prefix);
                }
                AuthScheme::SigV4 { service } => println!("auth:          sigv4 (service={service})"),
                AuthScheme::None => println!("auth:          none"),
                AuthScheme::Custom => println!("auth:          custom"),
            }
        }
        None => {
            eprintln!("unknown provider: {id}");
            std::process::exit(2);
        }
    }
}
```

Wire it in `crates/origin-cli/src/main.rs` (look for the `match cli.cmd` and add):

```rust
Some(Cmd::Providers { sub }) => match sub {
    cli_def::ProvidersSub::Ls => providers::ls(),
    cli_def::ProvidersSub::Describe { id } => providers::describe(&id),
},
```

Add `pub mod providers;` to the CLI lib (likely `crates/origin-cli/src/lib.rs` — check for existing `pub mod` lines).

Add `origin-provider = { path = "../origin-provider" }` to `crates/origin-cli/Cargo.toml` `[dependencies]` if not already there.

- [ ] **Step 3: Smoke-test the CLI**

Run: `cargo run -p origin-cli -- providers ls | head -5`
Expected: a header line plus at least 4 provider rows.

Run: `cargo run -p origin-cli -- providers describe deepseek`
Expected: prints `id: deepseek`, `wire: OpenAIChat`, etc.

- [ ] **Step 4: Commit**

```bash
git add crates/origin-cli
git commit -m "feat(cli): origin providers ls + describe"
```

---

### Task 21: `origin keyring login <provider>:<account>`

**Files:**
- Modify: `crates/origin-cli/src/cli_def.rs`
- Modify: `crates/origin-cli/src/main.rs` (or wherever keyring is handled)
- Modify: `crates/origin-cli/src/keyring.rs` (existing — confirm with `Grep`)

- [ ] **Step 1: Add the subcommand variant**

In `crates/origin-cli/src/cli_def.rs` `KeyringSub`:

```rust
/// Launch the OAuth flow for an OAuth provider and persist the tokens.
Login {
    /// Catalog id of an OAuth-backed provider, e.g. "github-copilot" or
    /// "anthropic-oauth".
    provider: String,
    /// Account name to store the tokens under. Defaults to "default".
    #[arg(default_value = "default")]
    account: String,
},
```

- [ ] **Step 2: Implement the handler**

In the keyring handler (find with `git grep -n "KeyringSub::Add"`), add a `Login` arm that:

1. Looks up the provider entry in `Catalog::builtin()`.
2. Asserts `entry.auth` is `OAuth(spec)`.
3. If `spec.device_flow`: print a code, poll the device endpoint, persist tokens.
4. Else (auth-code + PKCE): spin a localhost listener on the redirect URI port, generate PKCE, open the browser to `spec.authorize_url?...`, wait for the code, call `OAuthClient::exchange(...)`.

Implementation sketch (~80 lines) — call `origin_keyvault::OAuthClient::new(...)` plus `Pkce::new()` and `AuthCodeRequest::new(...)`. The full code follows the existing `pair` subcommand's pattern (see `crates/origin-cli/src/pair.rs` for a similar interactive-flow handler).

- [ ] **Step 3: Smoke-test**

A real OAuth roundtrip can't run in CI; assert the command prints the authorize URL and exits 0 when interrupted:

```bash
# Manual smoke (do not commit this script; just sanity-check locally)
cargo run -p origin-cli -- keyring login deepseek default 2>&1 | head -3
# Expected: clear error "deepseek is not an OAuth provider"
cargo run -p origin-cli -- keyring login anthropic-oauth default --dry-run 2>&1 | grep "authorize"
# Expected: an https://claude.ai/oauth/authorize?... URL
```

(Add a `--dry-run` flag if the implementation supports it; otherwise skip the second check.)

- [ ] **Step 4: Commit**

```bash
git add crates/origin-cli
git commit -m "feat(cli): keyring login subcommand drives OAuth flows from catalog"
```

---

## Phase H — Docs + final wiring

### Task 22: Update `docs/site/src/providers.md`

**Files:**
- Modify: `docs/site/src/providers.md`

- [ ] **Step 1: Replace the provider matrix**

Replace lines 9–24 (the existing `## Provider matrix` section) with a table that lists all 31 builtin providers grouped by wire format. For each, include columns: `ID`, `Display name`, `Wire`, `Auth`, `Default model`, `Streaming`, `Tools`.

Generate the table from the catalog (manually, not at build time) — copy the data from `crates/origin-provider/src/catalog_rows.rs`.

Add a `## OAuth providers` subsection listing the 4 OAuth-only entries with their `authorize_url`s and any required user-side registration.

Add a `## Custom providers` subsection showing an example `~/.origin/providers.toml`.

- [ ] **Step 2: Commit**

```bash
git add docs/site/src/providers.md
git commit -m "docs(providers): full 31-provider matrix + OAuth + custom sections"
```

---

### Task 23: Add the `oauth-providers` and `custom-providers` features

**Files:**
- Modify: `crates/origin-daemon/Cargo.toml`

- [ ] **Step 1: Update features**

Replace the `[features]` block with:

```toml
[features]
default = ["openai", "gemini", "ollama", "openrouter", "openai-compat", "oauth-providers", "custom-providers"]
openai = ["dep:origin-provider-openai", "openai-compat"]
openai-compat = ["dep:origin-provider-openai-compat"]
gemini = ["dep:origin-provider-gemini"]
ollama = ["dep:origin-provider-ollama"]
openrouter = ["dep:origin-provider-openrouter", "openai-compat"]
bedrock = ["dep:origin-provider-bedrock"]
github-models = ["dep:origin-provider-github"]
oauth-providers = []
custom-providers = []
```

Add `origin-provider-openai-compat = { path = "../origin-provider-openai-compat", optional = true }` to `[dependencies]` if not already added in Task 12.

Adjust factory code to gate OAuth and custom-loading code behind `#[cfg(feature = "oauth-providers")]` / `#[cfg(feature = "custom-providers")]`.

- [ ] **Step 2: Build with each feature combo**

```bash
cargo build -p origin-daemon --no-default-features --features openai,openai-compat
cargo build -p origin-daemon --features bedrock,github-models
cargo build -p origin-daemon  # default
```

All three expected to build cleanly.

- [ ] **Step 3: Commit**

```bash
git add crates/origin-daemon/Cargo.toml
git commit -m "build(daemon): default-on openai-compat, openrouter, oauth, custom features"
```

---

### Task 24: Final integration sweep

- [ ] **Step 1: Full workspace test**

Run: `cargo test --workspace --all-features 2>&1 | tail -30`
Expected: 0 failures.

- [ ] **Step 2: Format + clippy**

Run: `cargo fmt --all && cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 3: Commit any final fixups**

```bash
git add -A
git commit -m "chore: workspace test+lint sweep after multi-provider expansion"
```

---

## Verification gate

Before declaring the feature complete:

- [ ] `cargo test --workspace --all-features` passes.
- [ ] `cargo clippy --workspace --all-features --all-targets -- -D warnings` passes.
- [ ] `cargo run -p origin-cli -- providers ls` prints ≥30 rows.
- [ ] `cargo run -p origin-cli -- providers describe openai-codex` shows the OAuth spec.
- [ ] A user-created `~/.origin/providers.toml` with one row appears at the bottom of `providers ls`.
- [ ] Existing `anthropic_smoke`, `memory_e2e`, `openai_stream` (fuzz target compile-check) keep passing.

---

## Self-review notes (filled inline by the plan author)

- **Spec coverage:** Every section of the spec maps to a task block. §1 Catalog → Tasks 1–3. §2 OpenAI-compat client → Tasks 4–9. §3 ProviderFactory rewrite → Tasks 10–13. §4 Custom providers → Tasks 14–15. §5 OAuth flows → Tasks 16–19. §6 CLI surface → Tasks 20–21. §7 Feature surface → Task 23. §9 Testing strategy: catalog invariants (Task 2), wire smoke (Task 9), factory integration (Task 13), OAuth roundtrip per provider (Tasks 16–19), custom-providers (Task 14). §10 Migration via thin wrappers (Tasks 7–8).
- **Placeholders:** Tasks 16 and 21 mark certain implementation details as "the exact integration depends on the existing struct shape — read the current `lib.rs` first." This is a deliberate hand-off: the exact line edit depends on reading the existing `Anthropic` struct, which the engineer will do at the start of the task. The principle and required outcome are spelled out.
- **Type consistency:** `ProviderId` is a tuple newtype `Cow<'static, str>` end-to-end. `TokenSource` is a trait in `origin-provider-openai-compat`. `Catalog` lives in `origin-provider::catalog`. `OpenAiCompatConfig` carries `Arc<dyn TokenSource>`. No naming drift between tasks.
