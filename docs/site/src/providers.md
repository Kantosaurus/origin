# Providers

`origin` supports every major frontier provider through `origin-provider`'s
trait-per-provider design. A superset IR — `Message { role, blocks: Vec<Block> }`
— is projected to each provider's wire format at compile time via a
`#[derive(ProviderProjection)]` macro, so there is no lowest-common-denominator
loss and no `serde_json::Value` on the hot path.

## Provider matrix

The default `origin-cli` build ships Anthropic, OpenAI, Gemini, and Ollama.
Bedrock, OpenRouter, and GitHub Models are opt-in feature flags
(`--features bedrock,openrouter,github-models`).

| Provider | Auth | Streaming | Prompt cache | Thinking | Tools |
|---|---|---|---|---|---|
| Anthropic | API key / OAuth | Event-stream | Explicit (`cache_control`) | Native | Parallel |
| OpenAI | API key | SSE | Implicit + Beta API | Reasoning models | Parallel |
| Gemini | API key / ADC | SSE | Implicit context cache | Thought process | Parallel |
| Bedrock | SigV4 | Event-stream (binary) | Pass-through Anthropic | Pass-through | Parallel |
| OpenRouter | API key | SSE | Provider-dependent | Provider-dependent | Provider-dependent |
| Ollama | Local socket | NDJSON | n/a | n/a | Single |
| GitHub Models | OAuth | SSE | Implicit | Reasoning models | Parallel |

The `CachePlanner` adapts per-provider: if `cache_read_input_tokens` stays at
zero for sections marked cacheable on a given provider, the planner
auto-disables markers for that provider and re-plans on the next turn.

## Auth notes per provider

- **Anthropic.** API key (`x-api-key`) or OAuth via the Claude.ai pairing
  flow. OAuth refresh runs in the background; sessions can pin a specific
  account so multi-account users don't accidentally bill the wrong one.
- **OpenAI.** API key only. Organization and project IDs supported.
- **Gemini.** API key from AI Studio, or Application Default Credentials
  (ADC) — useful when running on GCE/GKE with a service-account identity.
- **Bedrock.** AWS SigV4 from the standard credential chain (env, profile,
  IMDS). `aws_region` required.
- **OpenRouter.** API key plus optional `referer` / `x-title` headers for
  the leaderboard.
- **Ollama.** Local socket; no auth. The provider only consumes the
  `endpoint` config key.
- **GitHub Models.** Device-flow OAuth via the `gh-models` scope; refresh
  rotates through KeyVault.

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
origin keyring login anthropic:claude-oauth
origin keyring login github-models

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
