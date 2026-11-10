# Environment Variables

Environment variables read across the `origin` workspace, grouped by area.
Found by grepping for `env::var` / `env::var_os` and the provider/API-key
patterns across all crates and packaging. The list is not exhaustive (for
example the swarm's `ORIGIN_SWARM_*` caps are not listed here; a few —
`ORIGIN_SWARM_MAX`, `ORIGIN_SWARM_LANE_MAX`, `ORIGIN_SWARM_BG`,
`ORIGIN_SWARM_BG_MAX` — are mentioned in the [CHANGELOG](../../CHANGELOG.md),
while `ORIGIN_SWARM_HEADROOM_MB`/`_PCT`, `ORIGIN_SWARM_RESERVE_MB`,
`ORIGIN_SWARM_POLL_MS`, `ORIGIN_SWARM_MEM_GOVERNOR` and `ORIGIN_SWARM_COLLAB`
are documented only in their source); every variable the harness-improvements
work (CHANGELOG `[Unreleased]`) added or changed is here, with its default as
read from the code.

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
| `ORIGIN_SIDECAR_MODEL` | daemon | Model for the cheap sidecar (NL summaries). On the Anthropic provider it is also the C4 `WebFetch` quarantine model; an empty value counts as unset. | `claude-haiku-4-5-20251001`. |
| `ORIGIN_DEFAULT_WORKFLOW` | daemon | Default-workflow directive switch (B5). `off`/`false`/`0` removes it everywhere; `on`/`true`/`1` includes it even in headless runs (`origin run`, self-dispatch). Swarm workers never get it (they use a fixed worker prompt). Read on every run. | Unset ⇒ interactive (TUI) sessions only. |
| `ORIGIN_DEFAULT_EFFORT` | daemon | Default reasoning effort for the main agent loop when the session set none (`/effort`, `/fast`, `--effort` win) (B3). Accepts the `/effort` names: `fast`, `low`/`min`, `medium`/`mid`, `high`, `max`/`maximum`/`ultra`, `ultracode`/`ultra-code`. Empty counts as unset; an unknown value is ignored with a warning. Read once per daemon process. Does not affect swarm sub-agents. | Unset ⇒ `high` on Anthropic, no effort on other providers (sub-agents: `medium` on Anthropic, or the parent's explicit effort). Set `max` to restore the pre-B3 default. |
| `ORIGIN_SWARM_ALWAYS` | daemon | `1` restores the "SWARM IS ALWAYS ON" and mandatory separate-verifier-`Task` paragraphs in the default-workflow directive (D3). Read once per daemon process. | Unset ⇒ mandates omitted. |
| `ORIGIN_ROUTER` | daemon | Per-turn model routing strategy (`auto`, plus the strategies in `routing.rs`; companions such as `ORIGIN_ROUTER_PLAN` / `ORIGIN_ROUTER_FAST`). Kept opt-in (D4): a mid-conversation model switch re-reads the prompt uncached. | Unset ⇒ routing off. |

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
| `ORIGIN_CHECKPOINTS` | daemon (agent) | `1` ⇒ enable per-turn checkpoints (shadow repo `<run dir>/.origin/shadow.git`). C8: when checkpoints are on (this or `ORIGIN_CHECKPOINTS_PER_TOOL`), a passing turn-end gate also moves the shadow ref `refs/tags/last-green` to a checkpoint of the passing tree, provided every armed gate ran and none was red. If the run then ends red, the final output (not the conversation, and not a sub-agent's result) says so and lists, to run in the run's directory: `origin checkpoint-diff <id>` to inspect, `origin rewind <id> --files-only` to restore the checkpoint's files, and `origin rewind <id>` to restore the whole tree and move the checkpoint history back. Restoring overwrites current files and discards later changes; a full `origin rewind` needs git 2.23 or newer (`git restore`). | Off. |
| `ORIGIN_CHECKPOINTS_PER_TOOL` | daemon (agent) | `1` ⇒ checkpoint per tool call. | Off. |
| `ORIGIN_COMPACT_SOFT_CAP` | daemon (agent) | Soft byte cap before context compaction. | Built-in default. |
| `ORIGIN_ELIDE_HIGH_WATER` | daemon (agent) | Percent (1–100) of the model's context window past which old tool results are replaced by one-line `[tool result elided: <tool> <N> bytes]` placeholders, in one batch sized to bring the context back under 50% of the window (A7). `0` disables high-water elision; the one-shot elide-and-retry on a provider context-length error stays active. | `70`. |
| `ORIGIN_SCHEMA_CRUSH` | daemon (agent) | `SchemaCrush` array compaction. | **On** (disable with `0`/`false`). |
| `ORIGIN_CMD_GUARD` | daemon (agent) | `0` ⇒ disable the Bash command guard. The guard refuses `Dangerous` commands and sends commands whose program cannot be determined statically (`NeedsApproval`), and installs of packages the project does not declare (C6), through the permission prompt (an ask the guard causes never offers "Always allow"); with no one to ask they are denied. Turn-end gate commands it rates `Dangerous` or `NeedsApproval` are not run: each run logs it, and the user is told once per session for each refused gate command with one `[origin]` line. `0` also turns off the C6 dependency-install gate. | **On.** |
| `ORIGIN_REPOMAP` | daemon (agent) | `0` disables the repo map. When it is on, the map is scanned once per session and workspace and reused unchanged (B2); entries for files a prompt names are added to that prompt's message. Swarm workers never get it. | **On** (opt out with `0`). |
| `ORIGIN_EDITFMT` | daemon (agent) | Edit-format override (C7); matched case-insensitively and read once per session at its first prompt. Unset, `1`, or an unrecognised value (warns): the per-model table picks the advertised edit tools — models it maps to whole-file (`gpt-3.5*`, `*turbo-instruct`) are not offered `Edit`/`MultiEdit`/`ApplyPatch` (`Write` stays); every other model keeps the full list. `0`/`off`/`false`/`search-replace`/`diff-fenced`/`udiff` ⇒ the full edit-tool list for every model. `whole-file` ⇒ no `Edit`/`MultiEdit`/`ApplyPatch` for any model. Exactly `1` additionally adds the `<origin-edit-format>` guidance block and the prose-edit applier (read per turn, so toggling it mid-session changes the prompt). A whole-file prose edit needs a path line (containing `/`, or a file name with an extension that has a letter; relative, no `..`) directly above a closed fenced block. Every prose edit is resolved against the run's working directory and applied only where a `Write` to that path would be allowed without asking (nothing in mode `on`, nothing outside the workspace or on protected paths; deny rules, policy, read-only mode, conseca paths and the `pre_tool` hook apply); edits not applied are named in one `[editfmt] … not applied` line. | Table on (changed in C7; was off). |
| `ORIGIN_TOOL_PROFILE` | daemon (agent) | `lean` ⇒ advertise only `Read`, `Edit`, `MultiEdit`, `ApplyPatch`, `Write`, `Bash`, `Grep`, `Glob`, `Recall`, `TodoWrite` (MCP/runtime tools unaffected) (C7); measured 5,510 vs 14,708 tool-definition bytes (10 vs 32 tools). `rich` ⇒ every builtin. Any other value ⇒ rich with a warning. Read once per session at its first prompt. Static prompt text still names some tools `lean` hides. | `rich`. |
| `ORIGIN_CODEGRAPH_AUTOBUILD` | daemon (agent) | `0` disables the automatic background code-graph build that a session's first `graph_query` starts (A11; gitignore-aware, stops at 2,000 indexable files, never in a run with its own working directory). The manual `graph_rebuild` tool is unaffected. | On. |
| `ORIGIN_TOOL_PROBE` | daemon (agent) | `0` omits the `Tools on PATH: …` line (toolchains and package managers found by a PATH lookup, no process spawned) from the `<origin-env>` system block (B6); on Unix a symlink to a directory is not counted as a tool. Read once per daemon process. | On. |
| `ORIGIN_EDIT_CHECK` | daemon (agent) | `0` disables the post-edit syntax gate: `py_compile`, `node --check` (`.js`/`.mjs`/`.cjs`; `.jsx` is skipped) and, since B7, the tree-sitter `<syntax-advisory>` (reports only parse errors an edit added). Per-turn test/lint gates still run when it is off. | On. |
| `ORIGIN_PRECOMPLETE_CHECK` | daemon (agent) | `1` enables the B11 soft pre-completion check: when the agent is about to finish after editing files with no command (Bash or a test/lint gate that ran) since the last edit, it is asked once to verify the change or explain why it cannot. Any other value is off. Never fires in read-only runs. | Off. |
| `ORIGIN_DIFF_REVIEW` | daemon (agent) | `1` turns on C6's per-edit regex tripwire notes (never block) and one end-of-run review of the run's diff by a tool-less model call with the session's provider and model (skipped beyond 2,000 changed lines; diff capped at 60,000 bytes; not on the last allowed turn). | Off. |
| `ORIGIN_REPRO_GATE` | daemon (agent) | `1`/`true`/`on` arms the reproduction/regression gate for the run: the `<repro-gate>` contract, a turn-end test run, and no tool-free "done" while tests are RED, with a test command auto-derived from the repo's markers. Any other non-off value is used verbatim as the test command. `0`/`false`/`off`/empty ⇒ off. C5: also runs the reproduction test before the model's first source edit and rejects one that passes, fails to load, or cannot be attributed (budget: `[post_edit] max_repair_iters` turns, separate from the terminal gate's). Kept opt-in (D1). | Off. |
| `ORIGIN_TEST_SELECT` | daemon (agent) | `1` narrows the gate's test command (pytest/go) using the code graph. Graph edges are file-local, so tests of callers in other files are not selected; it falls back to the full command when narrowing could drop coverage. | Off. |
| `ORIGIN_BESTOFN` | daemon (agent) | `N` (2–6; larger values are clamped to 6; `0`, `1` or non-numeric ⇒ off): for a hard instance with an armed test oracle (`ORIGIN_REPRO_GATE`), run N candidates in isolated git worktrees and apply the verified winner's diff. C5: candidates are scored on the pre-run tests; tests-only candidates never win; identical candidates are scored once; stops after two agreeing passes. Candidates run under the session's `[[permission_rules]]`, policy layers and conseca. The outcome is shown in one `[origin]` line (the applied candidate and why, or why none was applied; candidates that changed pre-existing tests, with paths; candidates excluded as tests-only). A winner whose files have uncommitted changes in the workspace is not applied; when it is applied next to other uncommitted changes, the test gate is re-run once on the real tree and the line names those files and the result. Not started in permission mode `on`. Kept opt-in (D2). | Off. |
| `ORIGIN_AUTOFORMAT` | daemon (agent) | `1` ⇒ auto-format files after edits. | Off. |
| `ORIGIN_AGENTGREP_TRUNCATE` | daemon (agent) | `1` ⇒ truncate agentgrep output. | Off. |
| `ORIGIN_BROWSER_VISUAL` | daemon (agent) | `1` ⇒ visual/headed browser mode. | Off (headless). |

## LSP & diagnostics

| Variable | Used by | Purpose | Default / notes |
|----------|---------|---------|-----------------|
| `ORIGIN_LSP_AUTO` | cli, daemon | If set, auto-start the LSP fleet (the daemon's auto-installer runs with the allow-listed child environment, A6). | Off. |
| `ORIGIN_LSP_DIAGNOSTICS` | daemon | `1` enables post-edit LSP diagnostics injection (C10). Language servers are kept warm across a prompt's turns, restarted after a turn with a shell or unlisted mutation and at each new prompt (also each `/goal` iteration), and killed when the session ends. Timeouts are reported (`diagnostics timed out after 2.5s`, `the language server is still starting`); a server that fails to start 3 times is not retried. Best-of-N candidate runs get no diagnostics. | Off. |

## Prompt caching

| Variable | Used by | Purpose | Default / notes |
|----------|---------|---------|-----------------|
| `ORIGIN_SYSTEM_CACHE` | provider-anthropic | `0` sends the Anthropic `system` prompt as a plain string with no `cache_control` marker (the pre-B2 wire shape), leaving all four markers to the messages. Use it for Anthropic-compatible endpoints that reject array-form or marked `system`. | Marked block array (B2). |
| `ORIGIN_CACHE_TTL_1H` | provider-anthropic | `1` ⇒ 1-hour cache TTL; since B2 the system-prompt marker gets it too. | Off. |

## Security, permissions & child processes

| Variable | Used by | Purpose | Default / notes |
|----------|---------|---------|-----------------|
| `ORIGIN_ALLOW_ALL` | cli (`origin run`), daemon | `1` runs every tool call without asking (permission mode `off`). Read by `origin run`, which sends it with its request (same as `--dangerously-allow-all`). Set in the daemon's own environment it applies only to requests nobody can answer (headless, scheduled, ambient) and is logged once; an interactive session always uses its own `/permissions` mode. The CLI removes it from the environment of a daemon it starts. Explicit `[[permission_rules]]` deny rules still apply. `bench/regression/run.py` and `bench/swe/adapters/origin.sh` set it for their `origin run` child: run those only in a disposable container or VM. | Unset (mode `workspace`; headless runs deny calls that would ask). |
| `ORIGIN_ENV_PASSTHROUGH` | sandbox (all child spawns) | Comma-separated env var names passed through to child processes (Bash, gate commands, hooks, MCP servers, language servers, formatters/linters, notification commands, checkpoints, the browser sidecars) in addition to the built-in allow-list; a listed name is passed even if it looks like a secret. Typical candidates: `HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY`, `SSL_CERT_FILE`, and registry/proxy URL variables such as `GOPROXY`, `PIP_INDEX_URL`, `PIP_EXTRA_INDEX_URL`, `NPM_CONFIG_REGISTRY`, `UV_INDEX_URL` (kept off the default allow-list because they can embed `user:token@` credentials), and any `AGENT_BROWSER_*` configuration variable the browser sidecars need (list each name; otherwise they are dropped). | Unset. |
| `ORIGIN_ENV_SCRUB` | sandbox (all child spawns) | `0` disables child-env scrubbing so children inherit the daemon's full environment (pre-A6 behaviour). Built-in allow-list (case-insensitive, minus anything named like a credential): `PATH`, `HOME`, `USERPROFILE`, `TEMP`, `TMP`, `TMPDIR`, `LANG`, `LC_*`, `TERM`, `SystemRoot`, `ComSpec`, `PATHEXT`, `APPDATA`, `LOCALAPPDATA`, `ProgramData`, `ProgramFiles`, `ProgramFiles(x86)`, `windir`, `NUMBER_OF_PROCESSORS`, `PROCESSOR_ARCHITECTURE`, `SHELL`, `USER`, `USERNAME`, `CARGO_HOME`, `RUSTUP_HOME`, `RUSTUP_TOOLCHAIN`, `RUSTFLAGS`, `CARGO_TARGET_DIR`, `GOPATH`, `GOROOT`, `GOFLAGS`, `GOMODCACHE`, `GOPRIVATE`, `GONOSUMDB`, `GONOPROXY`, `GOWORK`, `JAVA_HOME`, `NVM_DIR`, `NODE_PATH`, `VIRTUAL_ENV`, `CONDA_PREFIX`, `PYTHONPATH`, `PYTHONHOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, `XDG_DATA_HOME`, `LD_LIBRARY_PATH`, `DYLD_LIBRARY_PATH`, `DOTNET_ROOT`, `CI`, `SSH_AUTH_SOCK`, `PKG_CONFIG_PATH`, `TZ`, `SDKROOT`, `DEVELOPER_DIR`, and, for the browser sidecars, `PLAYWRIGHT_BROWSERS_PATH`, `DISPLAY`, `WAYLAND_DISPLAY`, `XAUTHORITY`, `XDG_RUNTIME_DIR` (on the allow-list, so they reach every child, Bash included). | Scrubbing on. |
| `ORIGIN_SANDBOX` | sandbox | `off` (case-insensitive) is an explicit opt-out of OS confinement; `sandbox_status()` then reports `disabled_by_user`. No backend is enforced on any shipped build, so today it changes only the reported status. | Unset ⇒ status `unavailable`. |
| `ORIGIN_WEBFETCH_ALLOW_PRIVATE` | `origin-browser` (`WebFetch`) | `1` lets `WebFetch` reach loopback, link-local, private, unique-local and unspecified addresses (and use system proxies) (A9). | Unset ⇒ such targets are refused, checked after DNS resolution and on every redirect. |
| `ORIGIN_WEBFETCH_RAW` | daemon | `1` skips the C4 quarantine call: `WebFetch` returns the fetched page itself (still wrapped in the `<external>` provenance marker) instead of a cheap model's answer to `prompt`; `prompt` is then not required (nor when no cheap model resolves). | Unset (quarantine on when a cheap model resolves: Anthropic provider only). |
| `ORIGIN_SESSION_RETENTION_DAYS` | daemon | Opt-in: a positive whole number of days; on daemon start, sessions idle longer than this are deleted (transcript rows and resume token) in one transaction, and the pruned ids are logged at warn. `0`, unset, or an unparsable/negative value keeps every session. Content-store blobs are not deleted, so this limits history, not disk content. | Unset (keep forever). |

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
| `ORIGIN_STEALTH_FALLBACK` | `origin-browser` (router) | `1` enables the CloakBrowser fallback when the primary browser backend reports bot detection (D11). Without it the Cloak sidecar is never spawned and the bot-detected response is returned as is. Read once, when the browser router is first started (once per daemon process). | Off. |
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
| `BENCH_KILL_DAEMON` | `bench/swe/adapters/origin.sh`, `bench/regression/run.py` | Any non-empty value ⇒ `pkill -f origin-daemon` after each trial (not on Windows). This kills every `origin-daemon` on the host. | Off. |
| `ORIGIN_TEST_OIDC_SUBJECT` | cli (oidc tests) | Test OIDC subject token. | Test-only. |
| `SELF_UPDATE_WORKER_ENV` | cli (updater) | Internal marker for the self-update worker process. | Set by the updater. |

## Standard / OS variables consumed

`origin` also reads OS-standard variables indirectly: `CARGO_TARGET_DIR`,
`LOCALAPPDATA`, `XDG_CACHE_HOME`, and locale variables (`LANG`/`LC_*` via the
i18n locale resolver). These are not origin-specific.

---

**Table rows in this reference: 86.**

---

_Last reviewed against workspace version 0.9.14 plus the `[Unreleased]` changes._
