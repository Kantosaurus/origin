# Environment Variables

Every environment variable read across the `origin` workspace, grouped by area.
Found by grepping for `env::var` / `env::var_os` and the provider/API-key
patterns across all crates and packaging.

Conventions:

- Booleans are read as the literal string `"1"` unless noted (e.g.
  `ORIGIN_SCHEMA_CRUSH` treats anything but `0`/`false` as enabled).
- `*_API_KEY` secrets are read once at startup; prefer the keyvault
  (`origin keyring add …`) where available so secrets never sit in the
  environment.
- `Secret<T>` wrapping means a value is redacted from logs/telemetry.

See also: [`../guides/configuration.md`](../guides/configuration.md) ·
[`../guides/providers-setup.md`](../guides/providers-setup.md) ·
[`../security/security-model.md`](../security/security-model.md) ·
[ipc-protocol.md](ipc-protocol.md)

---

## Provider credentials & API keys

| Variable | Used by | Purpose | Default / notes |
|----------|---------|---------|-----------------|
| `ANTHROPIC_API_KEY` | `origin-daemon`, provider-anthropic | Anthropic API key for the default provider. | Falls back to keyvault; empty default tolerated at boot. |
| `ANTHROPIC_WORKSPACE_ID` | `origin-cli` (oidc/cli_def) | Optional Anthropic workspace id. | Unset ⇒ account default. |
| `<UPPER_ID>_API_KEY` | `origin-cli` providers | Generic per-provider key, e.g. `ACME_API_KEY` for provider `acme` (`-`→`_`, upper-cased). | Used when no vault entry exists. |
| `TAVILY_API_KEY` | `origin-browser` (`WebSearch`) | Tavily web-search key. | Legacy fallback after vault `tavily:default`. |
| `ORIGIN_TAVILY_KEY` | `origin-cli` search | Tavily key for the CLI search engine. | — |
| `ORIGIN_BRAVE_KEY` | `origin-cli` search | Brave Search key for the CLI search engine. | — |

> Provider keys for OpenAI-compat, Gemini, Bedrock, GitHub Models and Ollama are
> resolved through the keyvault and the generic `<UPPER_ID>_API_KEY` convention;
> see [`../guides/providers-setup.md`](../guides/providers-setup.md).

## Paths, home & storage

| Variable | Used by | Purpose | Default / notes |
|----------|---------|---------|-----------------|
| `ORIGIN_HOME` | cli, daemon (widely) | Root config/state directory. | Platform default (e.g. `~/.origin`). |
| `ORIGIN_SOCK` | cli, daemon | IPC socket / named-pipe path. | `default_path()` per platform. |
| `ORIGIN_DB` | cli, daemon | SQLite session/store DB path. | `default_db_path()`. |
| `ORIGIN_CAS_ROOT` | cli, daemon | Content-addressed store root. | `default_cas_root()`. |
| `ORIGIN_DATA` | cli (knowledge) | Data dir for knowledge/model assets. | Platform default. |
| `ORIGIN_CACHE` | daemon | Cache dir override. | Falls back to `LOCALAPPDATA` / `XDG_CACHE_HOME`. |
| `ORIGIN_WORKSPACE` | daemon | Explicit workspace root. | Inferred from cwd otherwise. |
| `ORIGIN_GOVERNANCE_PATH` | daemon (config) | Explicit governance/policy file path. | Else `ORIGIN_HOME`-relative. |
| `ORIGIN_MEM_MODEL_DIR` | cli, daemon | Directory holding the embedding model (MiniLM). | Required for local memory embeddings. |
| `LOCALAPPDATA` | daemon | Windows cache base. | OS-provided. |
| `XDG_CACHE_HOME` | daemon | Unix cache base. | OS-provided. |
| `CARGO_TARGET_DIR` | daemon | Resolve build target dir (self-dev/bench). | Cargo standard. |

## Model & provider selection

| Variable | Used by | Purpose | Default / notes |
|----------|---------|---------|-----------------|
| `ORIGIN_MODEL` | cli, daemon, review | Default model id. | e.g. `claude-fable-5`; review uses `claude-opus-4-8`. |
| `ORIGIN_PROVIDER` | daemon | Initial provider override. | Else config default. |
| `ORIGIN_ACCOUNT` | daemon | Initial account id. | `default`. |
| `ORIGIN_SIDECAR_MODEL` | daemon | Model for the cheap sidecar (NL summaries). | `claude-haiku-4-5-20251001`. |
| `ORIGIN_DEFAULT_WORKFLOW` | daemon | Default workflow to activate. | Unset ⇒ none. |

## Daemon, supervisor & lifecycle

| Variable | Used by | Purpose | Default / notes |
|----------|---------|---------|-----------------|
| `ORIGIN_NO_SUPERVISOR` | cli (daemon_launch) | `1` ⇒ launch daemon without the supervisor. | Off. |
| `ORIGIN_SKIP_INIT` | cli (main) | Skip first-run init when config absent. | Off. |
| `ORIGIN_NO_UPDATE` | cli (updater) | Disable self-update checks. | Off. |
| `ORIGIN_BEARER_TTL_SECS` | daemon (config) | Bearer token TTL. | Built-in default. |
| `ORIGIN_METRICS_BIND` | daemon | Bind address for the metrics endpoint. | Unset ⇒ disabled. |
| `ORIGIN_OTLP_ENDPOINT` | daemon | OTLP trace/metric export endpoint. | Unset ⇒ no export. |

## Telemetry & observability

| Variable | Used by | Purpose | Default / notes |
|----------|---------|---------|-----------------|
| `ORIGIN_TELEMETRY` | daemon | `1` ⇒ opt **in** to telemetry. | **Off by default** (local-first). |
| `DO_NOT_TRACK` | daemon | If set, forcibly disables telemetry. | Respected over opt-in. |
| `ORIGIN_OTEL_CAPTURE_CONTENT` | daemon | `1` ⇒ capture gen-AI prompt/response content in spans. | Off (privacy default). |
| `ORIGIN_TURN_COST` | cli (status) | `1` ⇒ show per-turn cost in the status line. | Off. |
| `ORIGIN_NOTIFY` | daemon | `1` ⇒ emit completion notifications. | Off. |
| `ORIGIN_NOTIFY_DESKTOP` | cli (tui) | `1` ⇒ desktop notifications. | Off. |

## Agent loop & context features

| Variable | Used by | Purpose | Default / notes |
|----------|---------|---------|-----------------|
| `ORIGIN_CHECKPOINTS` | daemon (agent) | `1` ⇒ enable per-turn checkpoints. | Off. |
| `ORIGIN_CHECKPOINTS_PER_TOOL` | daemon (agent) | `1` ⇒ checkpoint per tool call. | Off. |
| `ORIGIN_COMPACT_SOFT_CAP` | daemon (agent) | Soft byte cap before context compaction. | Built-in default. |
| `ORIGIN_SCHEMA_CRUSH` | daemon (agent) | `SchemaCrush` array compaction. | **On** (disable with `0`/`false`). |
| `ORIGIN_CMD_GUARD` | daemon (agent) | `1` ⇒ extra command-guard checks for `Bash`. | Off. |
| `ORIGIN_REPOMAP` | daemon (agent) | `1` ⇒ inject repo-map block into the prompt. | Off. |
| `ORIGIN_EDITFMT` | daemon (agent) | `1` ⇒ edit-format guidance / post-edit format. | Off. |
| `ORIGIN_AUTOFORMAT` | daemon (agent) | `1` ⇒ auto-format files after edits. | Off. |
| `ORIGIN_AGENTGREP_TRUNCATE` | daemon (agent) | `1` ⇒ truncate agentgrep output. | Off. |
| `ORIGIN_BROWSER_VISUAL` | daemon (agent) | `1` ⇒ visual/headed browser mode. | Off (headless). |

## LSP & diagnostics

| Variable | Used by | Purpose | Default / notes |
|----------|---------|---------|-----------------|
| `ORIGIN_LSP_AUTO` | cli, daemon | If set, auto-start the LSP fleet. | Off. |
| `ORIGIN_LSP_DIAGNOSTICS` | daemon | If set, enable LSP diagnostics injection. | Off. |

## Memory garden & ambient

| Variable | Used by | Purpose | Default / notes |
|----------|---------|---------|-----------------|
| `ORIGIN_MEM_GARDEN` | daemon | `1` ⇒ enable background memory gardening. | Off. |
| `ORIGIN_AMBIENT` | daemon (ambient) | `1` ⇒ enable ambient/overnight loop. | Off. |
| `ORIGIN_AMBIENT_IDLE_MS` | daemon (ambient) | Min idle ms before ambient work starts. | Built-in default. |

## Self-development (gated)

| Variable | Used by | Purpose | Default / notes |
|----------|---------|---------|-----------------|
| `ORIGIN_SELFDEV` | cli (gaps), daemon | `1` ⇒ enable the self-dev control plane (`SelfDev*` IPC verbs). | Off (no-op verbs otherwise). |

## Remote / QUIC transport

| Variable | Used by | Purpose | Default / notes |
|----------|---------|---------|-----------------|
| `ORIGIN_REMOTE_CLIENT_CERT_FILE` | cli (admin_url) | Client certificate for remote mTLS admin. | Required for remote URL admin. |
| `ORIGIN_REMOTE_CLIENT_KEY_FILE` | cli (admin_url) | Client private key for remote mTLS admin. | Required for remote URL admin. |

## Browser / search runtime

| Variable | Used by | Purpose | Default / notes |
|----------|---------|---------|-----------------|
| `ORIGIN_CLOAK_DIR` | `origin-browser` (cloak) | Override path to the CloakBrowser sidecar dir. | Else sibling of the current exe. |
| `ORIGIN_SEARCH_GROUND` | cli (search) | If set, ground search results. | Off. |
| `ORIGIN_STT_CMD` | cli (voice) | Speech-to-text command. | `whisper`. |

## TUI / input

| Variable | Used by | Purpose | Default / notes |
|----------|---------|---------|-----------------|
| `ORIGIN_VIM` | cli (input) | `1` ⇒ vim keybindings. | Off. |

## Benchmark & test harness

| Variable | Used by | Purpose | Default / notes |
|----------|---------|---------|-----------------|
| `ORIGIN_BIN` | bench, cli | Path to the `origin` binary under test. | Resolved at runtime. |
| `ORIGIN_BENCH_BIN` | cli (bench) | Bench binary override (tried before `ORIGIN_BIN`). | — |
| `ORIGIN_BENCH_TASKS` | cli (bench) | Bench task directory. | `bench/perf/tasks`. |
| `ORIGIN_TEST_OIDC_SUBJECT` | cli (oidc tests) | Test OIDC subject token. | Test-only. |
| `SELF_UPDATE_WORKER_ENV` | cli (updater) | Internal marker for the self-update worker process. | Set by the updater. |

## Standard / OS variables consumed

`origin` also reads OS-standard variables indirectly: `CARGO_TARGET_DIR`,
`LOCALAPPDATA`, `XDG_CACHE_HOME`, and locale variables (`LANG`/`LC_*` via the
i18n locale resolver). These are not origin-specific.

---

**Total origin-specific / provider variables documented: 50.**

---

_Last reviewed against workspace version 0.9.8._
