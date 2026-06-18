# FAQ

Practical questions about `origin`, grounded in the workspace. For deeper detail,
follow the cross-links.

See also: [glossary.md](glossary.md) · [tool-catalog.md](tool-catalog.md) ·
[environment-variables.md](environment-variables.md) ·
[ipc-protocol.md](ipc-protocol.md) ·
[`../architecture/overview.md`](../architecture/overview.md) ·
[`../security/security-model.md`](../security/security-model.md)

---

## What is origin?

origin is a **local-first, Rust-native agentic coding assistant**. It runs as a
long-lived **daemon** (`origin-daemon`) that hosts the agent loop and serves a
CLI/TUI over a local socket (named pipe on Windows). State lives in a
**content-addressed store (CAS)**, a **code graph**, and a SQLite store on your
machine. See [`../architecture/overview.md`](../architecture/overview.md).

## How is origin different from other coding agents?

| Property | What it means |
|----------|---------------|
| **Performance-as-a-gate** | Latency/throughput budgets are first-class; features that regress them don't ship. The two-runtime split keeps latency-sensitive I/O from being starved by heavy work. |
| **Two-runtime model** | Separate async runtimes isolate UI/IPC latency from CPU-heavy agent/graph work. See [`../architecture/runtime-and-concurrency.md`](../architecture/runtime-and-concurrency.md). |
| **Archived IR** | Session/graph state is serialized as `rkyv`-archived IR that can be memory-mapped and read with zero deserialization — fast warm starts. See [`../architecture/data-and-storage.md`](../architecture/data-and-storage.md). |
| **Content-addressed everything** | Tool outputs, transcripts and IR are stored once by content hash and referenced by handle, deduplicated with FastCDC. Large results travel as CAS handles inflated by `Recall`. |
| **Stable prompt prefix** | Only hot tools embed full schemas; the rest are deferred behind `ToolSearch`, maximizing provider prompt-cache hits. |

## Is my code or data sent anywhere?

origin is **local-first**:

- Your files, sessions, memory and code graph stay **on your machine** (CAS +
  SQLite under `ORIGIN_HOME` / `ORIGIN_DATA` / `ORIGIN_CAS_ROOT`).
- **Telemetry is opt-in and off by default.** It only runs when
  `ORIGIN_TELEMETRY=1`, and `DO_NOT_TRACK` disables it outright. Prompt/response
  *content* is never captured in spans unless you also set
  `ORIGIN_OTEL_CAPTURE_CONTENT=1`.
- **Model providers necessarily receive the prompts you send them** (your
  instructions plus the context the agent includes). That is inherent to using a
  hosted LLM. Use a local provider (Ollama) to keep everything on-device.

See [environment-variables.md](environment-variables.md) and
[`../security/security-model.md`](../security/security-model.md).

## Which model providers are supported?

Anthropic, OpenAI-compatible endpoints, Google Gemini, AWS Bedrock, GitHub
Models, and local **Ollama**. The router selects provider/model/account per
request; the default model is set by `ORIGIN_MODEL` and a cheap **sidecar**
(default `claude-haiku-4-5`) handles NL summaries. Keys come from the keyvault or
the `<UPPER_ID>_API_KEY` convention (e.g. `ANTHROPIC_API_KEY`). See
[`../guides/providers-setup.md`](../guides/providers-setup.md) and
[`../subsystems/providers.md`](../subsystems/providers.md).

## How do skills work?

A **skill** is a named capability profile that masks the available tools
(`allowed-tools`) and primes the prompt. Skills are pushed/popped on a
per-connection stack (`ActivateSkill` / `DeactivateSkill`). **Workflows** chain
skill steps in dependency layers and persist to `workflows.toml`; run them
linearly with `{workflow:<name>}` or fan them out across the swarm with
`RunWorkflow`. See [`../subsystems/skills.md`](../subsystems/skills.md) and
[`../guides/authoring-skills.md`](../guides/authoring-skills.md).

## How do permissions and the sandbox protect me?

Two layers:

1. **Permission tiers** — every tool is `AutoAllowed` (read-only/low-risk, runs
   silently) or `RequiresPermission`. In interactive mode the daemon emits a
   `PermissionAsk` and blocks for your `PermissionDecision` (Allow once / Always
   / Deny). In headless/swarm runs the policy/governance engine decides
   (`ORIGIN_GOVERNANCE_PATH`).
2. **Sandbox profiles** — each tool runs child processes under a confinement
   profile: `Inherit`, `ReadFs` (read-only), `WriteCwd` (writes confined to the
   workspace), or `Shell` (full). Editing tools use `WriteCwd`; `Bash` uses
   `Shell`; `Read` uses `ReadFs`.

Secrets are wrapped in `Secret<T>` and redacted from logs/telemetry. See
[tool-catalog.md](tool-catalog.md), [`../crates/origin-permission.md`](../crates/origin-permission.md)
and [`../crates/origin-sandbox.md`](../crates/origin-sandbox.md).

## Can origin run autonomously?

Yes, with guardrails:

- **Goal driver** — give it a goal and the loop iterates until done, cancelled,
  or cleared (`Interrupt` / `ClearAll`). See [`../crates/origin-goal.md`](../crates/origin-goal.md).
- **Ambient / overnight** — a background loop (`ORIGIN_AMBIENT=1`,
  `ORIGIN_AMBIENT_IDLE_MS`) does useful work during idle time. See
  [`../crates/origin-ambient.md`](../crates/origin-ambient.md).
- **Swarm** — `Task`/`RunWorkflow` dispatch concurrent sub-agents, each confined
  to its own `allowed_tools` and budget, returning a `CompletionReport`.
- **Self-development** — a gated control plane (`ORIGIN_SELFDEV=1`) runs a
  supervised edit → checkpoint → build → test → restart cycle with operator
  approval. Off by default. See [`../crates/origin-selfdev.md`](../crates/origin-selfdev.md).

Autonomy still flows through the same permission/sandbox gates and the
supervisor's restart/replay machinery.

## How do I extend origin?

| Mechanism | Use it for | Reference |
|-----------|-----------|-----------|
| **Skills** | Curated tool sets + prompt priming for a task. | [`../guides/authoring-skills.md`](../guides/authoring-skills.md) |
| **Workflows** | Multi-step, layered skill pipelines (`AuthorWorkflow`). | [`../crates/origin-workflowgen.md`](../crates/origin-workflowgen.md) |
| **Builtin tools** | New first-party tools via the `origin_tool!` macro + inventory. | [`../crates/origin-tools.md`](../crates/origin-tools.md) |
| **MCP servers** | External tools/context over the Model Context Protocol (appear as `DynTool`). | [`../crates/origin-mcp.md`](../crates/origin-mcp.md) |
| **Plugins** | Packaged extensions. | [`../crates/origin-plugin.md`](../crates/origin-plugin.md) |
| **Hooks** | Lifecycle-event actions (pre/post tool, turn end). | [`../crates/origin-hooks.md`](../crates/origin-hooks.md) |

## Does origin work on Windows?

Yes. The IPC transport uses a **named pipe** on Windows (Unix domain socket
elsewhere) via the `interprocess` crate; cache/home resolution honours
`LOCALAPPDATA`. The local-socket reclaim-stale-socket logic is Unix-specific, but
the framing, daemon protocol, and tools are cross-platform. See
[ipc-protocol.md](ipc-protocol.md) and [`../guides/getting-started.md`](../guides/getting-started.md).

## How do I talk to the daemon directly?

Clients exchange length-prefixed frames (`MAGIC = "ORON"`,
`FrameKind ∈ {Request, Response, Event, ErrorFrame}`). The request body is a
`ClientMessage`; the daemon streams `StreamEvent`s and a terminal
`ServerMessage`. Remote access uses QUIC + mutual TLS with SHA-256 cert-fingerprint
pinning. See [ipc-protocol.md](ipc-protocol.md).

## What is the license?

origin is licensed under **Apache-2.0**. Every source file carries an
`SPDX-License-Identifier: Apache-2.0` header. See the repository `LICENSE` and
`CONTRIBUTING.md`.

## How large can a tool result or IPC payload be?

A single IPC frame body is capped at **64 MiB** (`MAX_FRAME_BYTES`); the reader
rejects an oversized advertised length *before* allocating, so a hostile peer
cannot trigger a giant allocation. In practice tool results are kept far smaller:
large outputs are stored in the CAS and returned as a **handle** that the model
inflates with `Recall`, array-heavy JSON is compacted by `SchemaCrush`, and each
builtin enforces its own `head_limit`. See [ipc-protocol.md](ipc-protocol.md)
and [tool-catalog.md](tool-catalog.md).

## Why does origin keep its prompt prefix stable?

Provider prompt-caching bills cached prefix tokens far cheaper than fresh ones.
origin embeds only **hot** tool schemas in the system prompt and defers the rest
behind `ToolSearch`, so the prefix rarely changes turn-to-turn. The
`Usage.cache_read_input_tokens` field reports how many tokens were served from
cache. See [glossary.md](glossary.md) and [`../crates/origin-cost.md`](../crates/origin-cost.md).

## How do I troubleshoot?

- Run `origin doctor` for environment checks ([`../crates/origin-doctor.md`](../crates/origin-doctor.md)).
- Daemon/supervisor lifecycle: [`../operations/daemon-and-supervisor.md`](../operations/daemon-and-supervisor.md).
- Observability/runbook: [`../operations/observability-runbook.md`](../operations/observability-runbook.md).
- Deterministic repro via replay + cassettes:
  [`../crates/origin-replay.md`](../crates/origin-replay.md).

---

_Last reviewed against workspace version 0.9.8._
