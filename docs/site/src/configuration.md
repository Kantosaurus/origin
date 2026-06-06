# Configuration

`origin` reads its persistent configuration from `~/.origin/config.toml` on
Linux/macOS and `%APPDATA%\origin\config.toml` on Windows. Environment
variables override the file. The daemon hot-reloads on `SIGHUP` (Unix) or via
`origin config reload`.

## Where things live

```
~/.origin/
├── config.toml          main config (this file)
├── skills/              user-installed skills (see Skills chapter)
├── hooks/               lifecycle hook scripts (see Hooks chapter)
├── cas/                 content-addressed pack files (mmap'd)
├── db/origin.sqlite     index database (SQLite WAL)
├── trace/               parquet trace ring (64MB rotation)
└── replay/              .origin-replay bundles
```

Nothing in `~/.origin/cas/` or `~/.origin/db/` is meant to be edited by hand
— `origin-cas` will hash-check on read and rebuild from session log replay if
a shard is corrupted.

## Sample `config.toml`

```toml
# Default model for new sessions. Per-session overrides via `origin run --model`.
[model]
default = "anthropic:claude-opus-4-7"
sidecar = "anthropic:claude-haiku-4"     # always-on small model
embedding = "local:minilm-l6-v2"          # bundled ONNX

# Providers. Credentials are NEVER stored here — they live in origin-keyvault
# (OS keychain). This block selects which providers are enabled and any
# non-secret per-account options.
[providers.anthropic]
enabled = true
default_account = "personal"

[providers.openai]
enabled = true

[providers.gemini]
enabled = false

[providers.ollama]
enabled = true
endpoint = "http://127.0.0.1:11434"

# Sandbox profiles per tool tier. See Troubleshooting for diagnosing denials.
[sandbox]
auto_allowed_profile = "strict"
requires_permission_profile = "user-widenable"
bash_cpu_seconds = 60
bash_memory_mb = 1024

# Where to find user extensions.
[paths]
skills = "~/.origin/skills"
hooks  = "~/.origin/hooks"

# Daemon runtime tuning. Defaults are calibrated for `physical_cores - 1`
# workers; override only if you know what you're doing.
[runtime]
worker_threads = 0                # 0 = auto
arena_hard_cap_mb = 256
cas_hot_lru_mb = 64

# Hooks shell pool — see Hooks chapter.
[hooks]
pool_size = 2
pre_tool_timeout_ms = 1000

# Telemetry is opt-in only. When false, the origin-telemetry-opt-in crate is
# not even linked.
[telemetry]
enabled = false
```

## Environment variables

A small set of env vars overrides config for ops use:

| Variable | Purpose |
|---|---|
| `ORIGIN_SOCK` | Override the IPC socket / named pipe path. Useful when running multiple daemons. |
| `ORIGIN_LOG` | `tracing-subscriber` filter, e.g. `origin_daemon=debug,hyper=warn`. |
| `ORIGIN_CONFIG` | Path to an alternate `config.toml` (whole-file override). |
| `ORIGIN_HOME` | Override `~/.origin` root. CI and ephemeral envs use this. |
| `ORIGIN_NO_SUPERVISOR` | Spawn `origin-daemon` directly instead of via `origin-supervisor` (the default auto-spawn). Disables crash-restart and self-dev binary hot-reload. |
| `ORIGIN_REPLAY_BUNDLE` | Load an `.origin-replay` bundle instead of hitting real providers. |

## Inspecting and editing from the CLI

`origin config` is a thin wrapper around the same TOML schema; it round-trips
through the daemon so a live session sees the change without a restart.

```bash
# Print every key
origin config get

# Single key (dotted path)
origin config get model.default

# Set a value (validated against the schema before write)
origin config set providers.openai.enabled true

# Reload after a manual edit
origin config reload
```

For values that should never appear in `config.toml` — API keys, OAuth
tokens, OS keyring entries — use `origin keyring` instead (see
[Providers](providers.md)).
