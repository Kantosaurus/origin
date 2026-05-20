# Providers

`origin` supports every major frontier provider through `origin-provider`'s
catalog-driven architecture. A static `ProviderEntry` catalog enumerates every
provider with its wire format, auth scheme, base URL, and capabilities. A single
`origin-provider-openai-compat` crate drives all OpenAI Chat-Completions shaped
providers; the four native wire formats (Anthropic, Gemini, Bedrock, Ollama) keep
their own crates.

## Provider matrix

### Native wire formats

| ID | Display name | Wire | Auth | Default model | Streaming | Tools |
|---|---|---|---|---|---|---|
| `anthropic` | Anthropic (API key) | Anthropic | api-key (`x-api-key`) | claude-sonnet-4-6 | yes | yes |
| `anthropic-oauth` | Anthropic (Claude CLI OAuth) | Anthropic | oauth (PKCE) | claude-sonnet-4-6 | yes | yes |
| `google` | Google (Gemini API key) | Gemini | api-key (`x-goog-api-key`) | gemini-2.5-pro | yes | yes |
| `gemini-oauth` | Gemini CLI OAuth | Gemini | oauth (PKCE) | gemini-2.5-pro | yes | yes |
| `bedrock` | AWS Bedrock | Bedrock | SigV4 | anthropic.claude-3-haiku-20240307-v1:0 | yes | yes |
| `ollama` | Ollama (local) | Ollama | none | llama3.2 | yes | no |
| `github-copilot` | GitHub Copilot | GitHubCopilot | oauth (device flow) | gpt-4o | yes | yes |

### OpenAI Chat-Completions compatible

| ID | Display name | Wire | Auth | Default model | Streaming | Tools |
|---|---|---|---|---|---|---|
| `openai` | OpenAI | OpenAIChat | api-key (Bearer) | gpt-4o | yes | yes |
| `openai-codex` | OpenAI Codex (ChatGPT OAuth) | OpenAIChat | oauth (PKCE) | gpt-5-codex | yes | yes |
| `openrouter` | OpenRouter | OpenAIChat | api-key (Bearer) | openrouter/auto | yes | yes |
| `deepseek` | DeepSeek | OpenAIChat | api-key (Bearer) | deepseek-chat | yes | yes |
| `fireworks` | Fireworks AI | OpenAIChat | api-key (Bearer) | accounts/fireworks/models/llama-v3p3-70b-instruct | yes | yes |
| `together` | Together AI | OpenAIChat | api-key (Bearer) | meta-llama/Llama-3.3-70B-Instruct-Turbo | yes | yes |
| `xai` | xAI (Grok) | OpenAIChat | api-key (Bearer) | grok-4 | yes | yes |
| `mistral` | Mistral AI | OpenAIChat | api-key (Bearer) | mistral-large-latest | yes | yes |
| `moonshot` | Moonshot AI (Kimi) | OpenAIChat | api-key (Bearer) | kimi-k2.5 | yes | yes |
| `minimax` | MiniMax | OpenAIChat | api-key (Bearer) | abab6.5s-chat | yes | yes |
| `stepfun` | StepFun | OpenAIChat | api-key (Bearer) | step-2-16k | yes | yes |
| `synthetic` | Synthetic | OpenAIChat | api-key (Bearer) | synthetic-coder | yes | yes |
| `venice` | Venice AI | OpenAIChat | api-key (Bearer) | default | yes | yes |
| `arcee` | Arcee AI | OpenAIChat | api-key (Bearer) | arcee-spark | yes | yes |
| `byteplus` | BytePlus | OpenAIChat | api-key (Bearer) | default | yes | yes |
| `chutes` | Chutes | OpenAIChat | api-key (Bearer) | default | yes | yes |
| `qwen` | Qwen Cloud (DashScope) | OpenAIChat | api-key (Bearer) | qwen-max | yes | yes |
| `qianfan` | Qianfan (Baidu) | OpenAIChat | api-key (Bearer) | ernie-4.5 | yes | yes |
| `volcano` | Volcano Engine | OpenAIChat | api-key (Bearer) | default | yes | yes |
| `xiaomi` | Xiaomi | OpenAIChat | api-key (Bearer) | mimo | yes | yes |
| `z-ai` | Z.AI (Zhipu GLM) | OpenAIChat | api-key (Bearer) | glm-4.6 | yes | yes |
| `ms-foundry` | Microsoft Foundry | OpenAIChat | api-key (`api-key`) | gpt-4o | yes | yes |
| `litellm` | LiteLLM Proxy | OpenAIChat | api-key (Bearer) | gpt-4o | yes | yes |
| `vercel-ai` | Vercel AI Gateway | OpenAIChat | api-key (Bearer) | openai/gpt-4o | yes | yes |
| `cloudflare` | Cloudflare AI Gateway | OpenAIChat | api-key (Bearer) | @cf/meta/llama-3.3-70b-instruct | yes | yes |
| `kilo` | Kilo Gateway | OpenAIChat | api-key (Bearer) | gpt-4o | yes | yes |
| `opencode` | OpenCode | OpenAIChat | api-key (Bearer) | default | yes | yes |
| `copilot-proxy` | Copilot Proxy | OpenAIChat | api-key (Bearer) | gpt-4o | yes | yes |
| `vllm` | vLLM | OpenAIChat | none | default | yes | yes |
| `sglang` | SGLang | OpenAIChat | none | default | yes | yes |
| `huggingface` | Hugging Face Inference | OpenAIChat | api-key (Bearer) | meta-llama/Llama-3.3-70B-Instruct | yes | yes |

The `CachePlanner` adapts per-provider: if `cache_read_input_tokens` stays at
zero for sections marked cacheable on a given provider, the planner
auto-disables markers for that provider and re-plans on the next turn.

## Auth notes per provider

- **Anthropic.** API key (`x-api-key`) or OAuth via the Claude.ai pairing
  flow. OAuth refresh runs in the background; sessions can pin a specific
  account so multi-account users don't accidentally bill the wrong one.
- **OpenAI.** API key (Bearer) or OAuth via `openai-codex` for ChatGPT accounts.
  Organization and project IDs supported.
- **Gemini.** API key from AI Studio (`x-goog-api-key`), or OAuth via `gemini-oauth`
  using the Google Accounts PKCE flow.
- **Bedrock.** AWS SigV4 from the standard credential chain (env, profile,
  IMDS). `aws_region` required.
- **OpenRouter.** API key plus optional `HTTP-Referer` / `X-Title` headers for
  the leaderboard (added automatically).
- **Ollama.** Local socket; no auth. The provider only consumes the
  `base_url` config key.
- **GitHub Copilot.** Device-flow OAuth — the CLI polls a device code endpoint,
  then exchanges for an access token that refresh-rotates through KeyVault.
- **Microsoft Foundry.** Uses an `api-key` header instead of `Authorization: Bearer`.
- **Cloudflare AI Gateway.** The `base_url` is a template containing
  `{account_id}` and `{gateway}` placeholders; store a JSON blob with these
  fields in the vault under `("cloudflare", "<account>/extras")`.
- **Local providers (vLLM, SGLang, LiteLLM).** No auth by default; set a Bearer
  token if your deployment requires one.

## OAuth providers

The four OAuth-backed providers in the builtin catalog:

### `anthropic-oauth` — Anthropic via Claude.ai PKCE flow

- **Authorize URL:** `https://claude.ai/oauth/authorize`
- **Token URL:** `https://console.anthropic.com/v1/oauth/token`
- **Client ID:** `9d1c250a-e61b-44d9-88ed-5944d1962f5e` (public Claude.ai pairing id)
- **Required scopes:** `org:create_api_key`, `user:profile`, `user:inference`
- **Redirect URI:** `https://console.anthropic.com/oauth/code/callback`
- **Flow:** PKCE auth-code (no device flow)

```bash
origin keyring login anthropic-oauth
```

### `gemini-oauth` — Gemini CLI via Google Accounts PKCE flow

- **Authorize URL:** `https://accounts.google.com/o/oauth2/v2/auth`
- **Token URL:** `https://oauth2.googleapis.com/token`
- **Client ID:** `681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com`
- **Required scopes:** `https://www.googleapis.com/auth/generative-language.retriever`
- **Redirect URI:** `http://localhost:8085`
- **Flow:** PKCE auth-code (no device flow)

```bash
origin keyring login gemini-oauth
```

### `github-copilot` — GitHub Copilot device flow

- **Device code URL:** `https://github.com/login/device/code`
- **Token URL:** `https://github.com/login/oauth/access_token`
- **Client ID:** `Iv1.b507a08c87ecfe98` (public Copilot client id)
- **Required scopes:** `read:user`
- **Flow:** Device flow (no PKCE, no browser redirect)

```bash
origin keyring login github-copilot
```

### `openai-codex` — OpenAI via ChatGPT PKCE flow

- **Authorize URL:** `https://auth.openai.com/oauth/authorize`
- **Token URL:** `https://auth.openai.com/oauth/token`
- **Client ID:** `app_EMoamEEZ73f0CkXaXp7hrann`
- **Required scopes:** `openid`, `profile`, `email`
- **Redirect URI:** `http://localhost:1455/auth/callback`
- **Flow:** PKCE auth-code (no device flow)

```bash
origin keyring login openai-codex
```

## Custom providers

You can add providers beyond the builtin catalog by creating
`~/.origin/providers.toml`. Entries are merged on top of the static catalog at
daemon startup; IDs must not collide with builtin entries.

```toml
[providers.my-gateway]
display_name = "My Gateway"
wire = "openai-chat"
base_url = "https://gw.example.com"
chat_path = "/v1/chat/completions"
default_model = "llama-3.3-70b"
auth = { kind = "api-key", header = "Authorization", prefix = "Bearer " }
```

Supported `wire` values: `openai-chat`, `anthropic`, `gemini`, `ollama`.

Supported `auth` kinds:

- `{ kind = "none" }` — no auth header (for local/self-hosted endpoints)
- `{ kind = "api-key", header = "Authorization", prefix = "Bearer " }` — any static key header

After adding an entry, set the credential:

```bash
origin keyring add my-gateway default <your-api-key>
```

Then use it:

```bash
ORIGIN_PROVIDER=my-gateway cargo run -p origin-cli -- run "hello"
```

## KeyVault: credentials never touch disk in plaintext

All credentials live in `origin-keyvault`, the single crate that touches OS
keychains. On Windows that's Credential Manager, on macOS Keychain, on
Linux Secret Service with an age-encrypted file fallback for headless
servers without a session bus. Multi-account per provider is first-class:

```bash
# Anthropic API key (interactive paste; never echoed to logs)
origin keyring set anthropic:work
origin keyring set anthropic:personal

# OAuth device flow (opens a browser, polls for completion)
origin keyring login anthropic-oauth
origin keyring login github-copilot

# List entries (names + last-used; never values)
origin keyring list

# Pin an account for the current session
origin run --provider anthropic --account work --prompt "..."
```

Swarm workers inherit their coordinator's account by default, so a parallel
multi-agent refactor doesn't fan out into N concurrent OAuth refreshes.

`Secret<T>` newtypes prevent leaks: `Debug` is `<redacted>`, and a CI lint
rejects any field whose name matches `*key*`, `*token*`, `*password*`, or
`*auth*` emitting raw bytes through `tracing`. Every keychain access is
logged to a 30-day audit ring kept separate from the regular trace parquet
ring — query it with:

```bash
origin keyring audit --since 24h
```

## Failure modes

- 429 → exponential backoff respecting `Retry-After`, with an optional
  fallback provider configured in `config.toml`.
- Schema drift → `TokenEvent::Unknown` logged, non-fatal; the projection
  macro will be regenerated in the next release.
- Cache-marker rejection → auto-disabled per provider with a warning in the
  `?metrics` side panel.
