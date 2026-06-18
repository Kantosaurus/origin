# Providers Setup

`origin` talks to LLM backends through a single provider catalog — **40+**
providers behind one uniform interface. This guide shows how to authenticate
each kind of provider, the env vars and flags involved, and how `origin` picks
a default model.

The cleanest path for any provider is the interactive wizard:

```sh
origin init                 # pick primary/backup/subagent + capture credentials
origin providers ls         # list every catalog entry (id, name, wire, auth)
origin providers describe <id>   # full config for one provider
```

`origin init` chooses the provider, captures the credential matching its auth
scheme, probes the credential against the provider's `/models` endpoint, and
writes your selection to `~/.origin/config.toml`. **Secrets go to your OS
keychain**, never the config file.

> For the wire-driver internals (the `Provider` trait, the openai-compat driver,
> shimquirks, routing, cost), see the
> [provider subsystem reference](../subsystems/providers.md).

---

## Authentication schemes

Every catalog entry declares one of five auth schemes. How you supply the
credential depends on the scheme:

| Scheme | How `origin init` captures it | Stored as |
| --- | --- | --- |
| **None** | No credential step | — |
| **API key** | Paste the key when prompted | keychain `provider:account` |
| **OAuth** | Runs the browser/device authorization flow | keychain OAuth blob, auto-refreshed |
| **AWS SigV4** | Prompts for access key id + secret access key | keychain JSON blob |
| **Custom** | Skipped — attach later with `origin keyring add` | keychain |

You can also manage credentials directly:

```sh
origin keyring add <provider> <account> <secret>   # API key / SigV4 (secret can be `-` for stdin)
origin keyring login <provider> [account]          # OAuth flow (e.g. github-copilot, anthropic-oauth)
origin keyring list <provider>                      # accounts for a provider
origin keyring remove <provider> <account>          # delete a credential
```

The `account` is almost always `default`.

---

## Native-wire providers

These speak a vendor-native API and have a dedicated wire driver.

### Anthropic (API key)

```sh
origin keyring add anthropic default sk-ant-...
```

- **id:** `anthropic` · **auth:** API key (sent as the `x-api-key` header).
- Supports prompt caching and extended thinking. The `--thinking-tokens` and
  `--effort` flags are honoured by this provider.
- Default model: `claude-fable-5`.

### Anthropic (Claude CLI OAuth)

```sh
origin keyring login anthropic-oauth
```

- **id:** `anthropic-oauth` · **auth:** OAuth (PKCE). Mirrors the Claude CLI
  client; tokens are refreshed automatically.

### Google Gemini (API key)

```sh
origin keyring add google default <gemini-key>
```

- **id:** `google` (the alias `gemini` resolves to it) · **auth:** API key (sent
  as the `x-goog-api-key` header / `?key=` query param). Default model:
  `gemini-2.5-pro`.

### Gemini CLI OAuth

- **id:** `gemini-oauth` · **auth:** OAuth. `origin keyring login gemini-oauth`.

### AWS Bedrock (SigV4)

```sh
origin init      # choose Bedrock, then enter the AWS access key id + secret
```

- **id:** `bedrock` (alias `aws-bedrock`) · **auth:** AWS SigV4. Credentials are
  stored as a JSON blob with `access`, `secret`, and `region`; `endpoint` and
  `model_id` default sensibly when omitted. Default model:
  `anthropic.claude-3-haiku-20240307-v1:0`. Opt-in cargo feature `bedrock`.

### Ollama (local — no auth)

```sh
# Just run a local Ollama daemon; no credential needed.
ollama serve
origin init      # choose Ollama, accept the default model
```

- **id:** `ollama` · **auth:** none. Talks to `http://localhost:11434/api/chat`.
  Override the endpoint via `~/.origin/providers.toml`. Default model:
  `llama3.2`. Opt-in cargo feature `ollama`.

### GitHub Copilot (OAuth device flow)

```sh
origin keyring login github-copilot
```

- **id:** `github-copilot` (aliases `github`, `github-models`) · **auth:** OAuth
  **device flow**. `origin` exchanges the stored GitHub token for a short-lived
  Copilot session token on each request. Opt-in cargo feature `github-models`.

---

## OpenAI & OpenAI-compatible providers

OpenAI itself and a long tail of OpenAI-shaped vendors ride one generic
Chat-Completions driver. For all of these, authentication is a **Bearer API
key** unless noted, and the base URL/path come from the catalog:

```sh
origin keyring add <id> default <api-key>
```

| id | Provider | Auth | Default model |
| --- | --- | --- | --- |
| `openai` | OpenAI | Bearer key | `gpt-4o` |
| `openai-codex` | OpenAI Codex (ChatGPT OAuth) | OAuth (Responses API) | `gpt-5-codex` |
| `openrouter` | OpenRouter | Bearer key | `openrouter/auto` |
| `deepseek` | DeepSeek | Bearer key | `deepseek-chat` |
| `fireworks` | Fireworks AI | Bearer key | `llama-v3p3-70b-instruct` |
| `together` | Together AI | Bearer key | `Llama-3.3-70B-Instruct-Turbo` |
| `xai` | xAI (Grok) | Bearer key | `grok-4` |
| `mistral` | Mistral AI | Bearer key | `mistral-large-latest` |
| `moonshot` | Moonshot AI (Kimi) | Bearer key | `kimi-k2.5` |
| `kimi` | Kimi Code (subscription) | Bearer key | `kimi-k2.5` |
| `minimax` | MiniMax | Bearer key | `abab6.5s-chat` |
| `stepfun` | StepFun | Bearer key | `step-2-16k` |
| `synthetic` | Synthetic | Bearer key | `synthetic-coder` |
| `venice` | Venice AI | Bearer key | `default` |
| `arcee` | Arcee AI | Bearer key | `arcee-spark` |
| `byteplus` | BytePlus | Bearer key | `default` |
| `chutes` | Chutes | Bearer key | `default` |
| `qwen` | Qwen Cloud (DashScope) | Bearer key | `qwen-max` |
| `qwen-intl` | Qwen Cloud (Global) | Bearer key | `qwen-max` |
| `qianfan` | Qianfan (Baidu) | Bearer key | `ernie-4.5` |
| `volcengine` | Volcano Engine | Bearer key | `default` |
| `xiaomi` | Xiaomi (MiMo) | Bearer key | `mimo-v2-flash` |
| `z-ai` | Z.AI (Zhipu GLM) | Bearer key | `glm-4.6` |
| `ms-foundry` | Microsoft Foundry | `api-key` header | `gpt-4o` |
| `groq` | Groq | Bearer key | `llama-3.3-70b-versatile` |
| `cerebras` | Cerebras | Bearer key | `llama-3.3-70b` |
| `deepinfra` | DeepInfra | Bearer key | `Llama-3.3-70B-Instruct` |
| `nvidia` | NVIDIA | Bearer key | `llama-3.3-70b-instruct` |
| `tencent` | Tencent Cloud (TokenHub) | Bearer key | `deepseek-v3` |
| `nebius` | Nebius AI Studio | Bearer key | `Llama-3.3-70B-Instruct` |
| `huggingface` | Hugging Face Inference | Bearer key | `Llama-3.3-70B-Instruct` |

### Gateways and proxies

| id | Provider | Auth | Notes |
| --- | --- | --- | --- |
| `litellm` | LiteLLM Proxy | Bearer (`LITELLM_API_KEY`) | Defaults to a local proxy `http://localhost:4000`; override for a remote deployment. |
| `vercel-ai` | Vercel AI Gateway | Bearer key | Default model `openai/gpt-4o`. |
| `cloudflare` | Cloudflare AI Gateway | Bearer key | Base URL has `{account_id}`/`{gateway}` placeholders — supply them via the `<account>/extras` vault blob. |
| `kilo` | Kilo Gateway | Bearer key | — |
| `opencode` | OpenCode | Bearer key | — |
| `copilot-proxy` | Copilot Proxy | Bearer key | — |

### Self-hosted servers (often no auth)

| id | Provider | Auth | Default base URL |
| --- | --- | --- | --- |
| `vllm` | vLLM | none | `http://localhost:8000` |
| `sglang` | SGLang | none | `http://localhost:30000` |
| `lmstudio` | LM Studio (local) | none | `http://localhost:1234` |

> The `openai-codex` entry uses the OpenAI **Responses** API (not
> Chat-Completions) and authenticates via ChatGPT **OAuth**:
> `origin keyring login openai-codex`.

---

## Custom providers (`providers.toml`)

If a vendor isn't in the builtin catalog but is OpenAI-shaped, add it to
`~/.origin/providers.toml` and it will be merged into the catalog (and reachable
from `origin init`'s "Other" picker). Each entry mirrors a catalog row:

```toml
[[providers]]
id = "my-vendor"
display_name = "My Vendor"
wire = "OpenAIChat"          # OpenAIChat | OpenAIResponses | Anthropic | Gemini | Bedrock | Ollama | GitHubCopilot
base_url = "https://api.my-vendor.example"
chat_path = "/v1/chat/completions"
default_model = "my-model"

[providers.auth]
# one of: None | ApiKey { header, prefix } | OAuth(...) | SigV4 { service } | Custom
ApiKey = { header = "Authorization", prefix = "Bearer " }
```

Custom rows are merged **atomically** — a single id collision leaves the catalog
unchanged. Then attach the credential:

```sh
origin keyring add my-vendor default <api-key>
```

A templated `base_url` (with `{placeholder}` segments, like Cloudflare's) is
expanded from a `<account>/extras` JSON blob in the vault.

---

## How a default model is chosen

`origin` resolves the model for a turn in this order:

1. **Explicit override** — `--model <id>` on `origin run`, or a `[primary]`
   `model` in `~/.origin/config.toml`. An `[aliases]` entry or `--alias` is
   substituted first.
2. **Provider catalog default** — each `ProviderEntry` carries a
   `default_model` (the table above). `origin init` pre-selects it.
3. **Runtime discovery** — `origin providers refresh` queries a custom
   provider's `/models` endpoint and caches the result to
   `~/.origin/models-cache.json`, so the picker can offer the live list.

`origin` can also **recommend** the cheapest capable model from a candidate set,
ranked by its builtin pricing table:

```sh
origin providers recommend                          # rank a builtin candidate set
origin providers recommend openai/gpt-4o anthropic/claude-fable-5 --write
```

`--write` saves the pick to `~/.origin/recommended.json`. For multi-model
routing strategies (architect/editor splits, phase-aware routing, quota
fallback), see the [provider subsystem reference](../subsystems/providers.md).

---

## Verifying a provider

```sh
origin doctor              # environment + connectivity diagnostics
origin providers ls        # confirm the provider is in the catalog
origin usage               # tokens in/out per provider/model
origin run "hello" --model <provider>/<model>   # smoke-test a specific model
```

If a credential probe fails during `origin init`, the wizard offers a retry loop
so you can re-paste the key without restarting the flow.

_Last reviewed against workspace version 0.9.8._
