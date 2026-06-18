# Glossary

Domain terms used across the `origin` workspace and docs. Each entry links to the
most relevant subsystem, crate, or reference doc. The table below is the
canonical A–Z reference; the sections after it group the terms by theme for
orientation.

## How to read this glossary

- **Storage terms** (CAS, IR, archived IR, FastCDC, Hot/Warm/Cold) describe how
  state is persisted and read back fast.
- **Runtime terms** (two-runtime model, `spawn_in`, task class) describe how work
  is scheduled.
- **Agent terms** (skill, workflow, swarm, goal driver, ambient, steering)
  describe how the assistant acts.
- **Safety terms** (permission tier, sandbox profile, `Secret<T>`, policy,
  pairing, fingerprint pinning) describe how you stay in control.
- **Protocol terms** (frame, FrameKind, ClientMessage, StreamEvent) describe the
  IPC wire format — see [ipc-protocol.md](ipc-protocol.md).

## A–Z reference

| Term | Definition |
|------|------------|
| **origin** | A local-first, Rust-native agentic coding assistant: a long-lived daemon plus CLI/TUI clients, content-addressed storage, a code graph, and a multi-provider model router. See [`../architecture/overview.md`](../architecture/overview.md). |
| **CAS** (content-addressed store) | Blob store keyed by content hash; tool results, transcripts and IR are stored once and referenced by handle. Inflated on demand with the `Recall` tool. See [`../crates/origin-cas.md`](../crates/origin-cas.md). |
| **CAS handle** | A lowercase hex hash naming a CAS blob. Tools return handles instead of large payloads; `Recall` re-inflates them (optionally a region). See [tool-catalog.md](tool-catalog.md). |
| **IR (rkyv)** | Intermediate representation serialized with `rkyv` for zero-copy reads. See [`../architecture/data-and-storage.md`](../architecture/data-and-storage.md). |
| **Archived IR** | The `rkyv`-archived form of IR that can be memory-mapped and read without deserialization — the basis of origin's fast cold-start. See [`../architecture/data-and-storage.md`](../architecture/data-and-storage.md). |
| **Hot / Warm / Cold tiers** | Storage/latency tiers: Hot = in-memory live state, Warm = mmap'd archived IR ready instantly, Cold = on-disk/CAS reconstructed on demand. See [`../architecture/data-and-storage.md`](../architecture/data-and-storage.md). |
| **FastCDC** | Content-defined chunking algorithm used to deduplicate and chunk blobs in the CAS. See [`../crates/origin-cas.md`](../crates/origin-cas.md). |
| **Task class** | Scheduling priority of a unit of work (e.g. `Critical`, normal); resumed tool calls re-spawn under `TaskClass::Critical`. See [`../architecture/runtime-and-concurrency.md`](../architecture/runtime-and-concurrency.md). |
| **spawn_in** | Runtime affinity hint controlling which executor a task is dispatched onto (the two-runtime split). See [`../architecture/runtime-and-concurrency.md`](../architecture/runtime-and-concurrency.md). |
| **Two-runtime model** | Separate async runtimes isolating latency-sensitive I/O from heavy CPU/agent work so one cannot starve the other. See [`../architecture/runtime-and-concurrency.md`](../architecture/runtime-and-concurrency.md). |
| **Skill** | A named, reusable capability profile that masks the tool set and primes the prompt; pushed/popped on a per-connection stack via `ActivateSkill`/`DeactivateSkill`. See [`../subsystems/skills.md`](../subsystems/skills.md). |
| **allowed-tools** | The explicit tool allow-list a skill or sub-agent is confined to; a sub-agent can only call tools in its `allowed_tools`. See [`../subsystems/skills.md`](../subsystems/skills.md). |
| **Hook** | A user-defined action fired on a lifecycle event (`HookEventKind` / `LifecycleEvent`) — e.g. pre/post tool, turn end. See [`../crates/origin-hooks.md`](../crates/origin-hooks.md). |
| **Workflow** | An ordered, dependency-layered set of skill steps persisted in `workflows.toml`; runnable linearly (`{workflow:<name>}`) or fanned out across the swarm via `RunWorkflow`. See [`../crates/origin-workflowgen.md`](../crates/origin-workflowgen.md). |
| **Swarm** | The pool of concurrent sub-agents (workers) dispatched by `Task`/`RunWorkflow`. See [`../subsystems/swarm-and-orchestration.md`](../subsystems/swarm-and-orchestration.md). |
| **Sub-agent / worker** | A confined agent spawned with a goal, an `allowed_tools` set and a budget; reports back a `CompletionReport`. See [`../crates/origin-swarm.md`](../crates/origin-swarm.md). |
| **CompletionReport** | The structured summary a sub-agent returns (outcome, artifacts, status). See [`../crates/origin-swarm.md`](../crates/origin-swarm.md). |
| **Goal driver** | The loop (`drive_goal_loop`) that iterates a long-running goal until done, cancelled, or cleared. See [`../crates/origin-goal.md`](../crates/origin-goal.md). |
| **Self-tag protocol** | The convention by which agents tag their own outputs/state so steering and orchestration can route and resume them. See [`../subsystems/swarm-and-orchestration.md`](../subsystems/swarm-and-orchestration.md). |
| **Permission tier** | A tool's risk classification — `AutoAllowed` (runs silently) or `RequiresPermission` (gated by a `PermissionAsk`). See [tool-catalog.md](tool-catalog.md) and [`../crates/origin-permission.md`](../crates/origin-permission.md). |
| **PermissionAsk / PermissionDecision** | The IPC event/reply pair that gates a `RequiresPermission` tool in interactive mode; `always` remembers the decision. See [ipc-protocol.md](ipc-protocol.md). |
| **Sandbox profile** | Per-tool process confinement (`Inherit`, `ReadFs`, `WriteCwd`, `Shell`) applied to children a tool spawns. See [`../crates/origin-sandbox.md`](../crates/origin-sandbox.md). |
| **Secret\<T\>** | A wrapper type that redacts its value from logs, debug output and telemetry. See [`../crates/origin-keyvault.md`](../crates/origin-keyvault.md). |
| **Keyvault** | OS-keychain-backed secret store for provider credentials; preferred over env vars. See [`../crates/origin-keyvault.md`](../crates/origin-keyvault.md). |
| **MCP** (Model Context Protocol) | Standard for external tool/context servers; MCP tools appear at runtime as `DynTool` objects. See [`../crates/origin-mcp.md`](../crates/origin-mcp.md). |
| **HNSW** | Hierarchical Navigable Small World graph — the approximate-nearest-neighbour index backing semantic memory search. See [`../subsystems/memory-and-codegraph.md`](../subsystems/memory-and-codegraph.md). |
| **MiniLM** | The small sentence-embedding model used locally to vectorize memory/knowledge (model dir set by `ORIGIN_MEM_MODEL_DIR`). See [`../crates/origin-mem.md`](../crates/origin-mem.md). |
| **Code graph** | A graph of code entities (defs, refs, calls, modules) supporting typed queries (`neighbors`, `path`, `communities`, `god_nodes`, `recent_changes`). See [`../crates/origin-codegraph.md`](../crates/origin-codegraph.md). |
| **God node** | A graph node with abnormally high connectivity (a central hub) surfaced by the `god_nodes` query — often a refactor hotspot. See [`../crates/origin-codegraph.md`](../crates/origin-codegraph.md). |
| **Community** | A densely-connected cluster of code-graph nodes; summarizable via `graph_summarize`. See [`../crates/origin-codegraph.md`](../crates/origin-codegraph.md). |
| **Repo map** | A compact, ranked overview of the repository injected into the prompt when `ORIGIN_REPOMAP=1`. See [`../crates/origin-repomap.md`](../crates/origin-repomap.md). |
| **Prompt-cache prefix planner** | Logic that keeps the system-prompt prefix stable (hot tools only, deferred schemas behind `ToolSearch`) to maximize provider prompt-cache hits. See [tool-catalog.md](tool-catalog.md). |
| **Hot tool** | A tool whose full schema is embedded in the system prompt (`hot: true`). See [tool-catalog.md](tool-catalog.md). |
| **Deferred tool** | A tool advertised as `{name, description}` only; its schema is fetched on demand via `ToolSearch`. See [tool-catalog.md](tool-catalog.md). |
| **SchemaCrush** | Array/JSON compaction (`array_crush`) bounding tool-result size before the model sees it. See [`../crates/origin-tools.md`](../crates/origin-tools.md). |
| **Sticky band** | A routing/affinity band that keeps related work pinned to the same context/worker for cache locality. See [`../subsystems/swarm-and-orchestration.md`](../subsystems/swarm-and-orchestration.md). |
| **Sidecar** | A cheap secondary model (default `claude-haiku-4-5`) used for NL summaries such as `graph_explain`. See [`../crates/origin-sidecar.md`](../crates/origin-sidecar.md). |
| **Supervisor** | The process (`origin-supervisor`) that launches, watches, and restarts the daemon, replaying `ResumeRequest`s. See [`../crates/origin-supervisor.md`](../crates/origin-supervisor.md) and [`../operations/daemon-and-supervisor.md`](../operations/daemon-and-supervisor.md). |
| **Daemon** | The long-lived `origin-daemon` process that runs the agent loop and serves IPC. See [`../crates/origin-daemon.md`](../crates/origin-daemon.md). |
| **Replay** | Deterministic re-execution of a recorded session for testing/debugging. See [`../crates/origin-replay.md`](../crates/origin-replay.md). |
| **Cassette** | A recorded provider/IPC interaction used by replay to drive a deterministic run without hitting live services. See [`../crates/origin-cassette.md`](../crates/origin-cassette.md). |
| **IPC tap** | A recorder hook that captures IPC frames into a cassette (feature-gated). See [ipc-protocol.md](ipc-protocol.md). |
| **Ambient / overnight** | A background loop (`ORIGIN_AMBIENT=1`) that does useful idle-time work after a configurable idle threshold. See [`../crates/origin-ambient.md`](../crates/origin-ambient.md). |
| **Steering** | Operator/orchestration influence over a running agent's direction mid-flight. See [`../crates/origin-steering.md`](../crates/origin-steering.md). |
| **Resume token** | A checkpoint (`ResumeToken`) describing how to rehydrate a session from CAS after a restart. See [`../crates/origin-resume-token.md`](../crates/origin-resume-token.md). |
| **Frame** | The length-prefixed IPC wire unit: `MAGIC | kind | request_id | body_len | body`. See [ipc-protocol.md](ipc-protocol.md). |
| **FrameKind** | The frame's type byte: `Request`(1), `Response`(2), `Event`(3), `ErrorFrame`(4). See [ipc-protocol.md](ipc-protocol.md). |
| **StreamEvent** | A daemon→client mid-turn event (text deltas, tool activity, swarm worker updates, permission/choice asks). See [ipc-protocol.md](ipc-protocol.md). |
| **ClientMessage** | A client→daemon request (prompt, decisions, admin verbs, skill/workflow control). See [ipc-protocol.md](ipc-protocol.md). |
| **Cert fingerprint pinning** | The SHA-256 certificate fingerprint that authenticates remote QUIC/mTLS peers, pinned at pairing time. See [ipc-protocol.md](ipc-protocol.md). |
| **Pairing** | The `PairStart`/`PairRedeem` handshake binding a remote device via a short-lived code. See [ipc-protocol.md](ipc-protocol.md). |
| **Router** | The component that selects provider/model/account per request. See [`../crates/origin-router.md`](../crates/origin-router.md). |
| **Provider** | A backend model service adapter (Anthropic, OpenAI-compat, Gemini, Bedrock, GitHub, Ollama). See [`../subsystems/providers.md`](../subsystems/providers.md). |
| **Policy / governance** | Rules deciding tool allow/deny in non-interactive runs (`ORIGIN_GOVERNANCE_PATH`). See [`../crates/origin-policy.md`](../crates/origin-policy.md). |
| **Self-dev** | The gated self-modification control plane (`ORIGIN_SELFDEV=1`): edit → checkpoint → build → test → restart. See [`../crates/origin-selfdev.md`](../crates/origin-selfdev.md). |
| **agentgrep DSL** | The opt-in `Grep` syntax: `agentgrep:outline:<path>` lists symbols; `agentgrep:refs:<symbol>` finds references with enclosing definitions. See [tool-catalog.md](tool-catalog.md). |
| **Telemetry (opt-in)** | Usage/observability data, **off by default**; enabled with `ORIGIN_TELEMETRY=1` and suppressed by `DO_NOT_TRACK`. See [environment-variables.md](environment-variables.md). |
| **Token usage / cache fields** | `Usage` events carry `cache_read_input_tokens` and `cache_creation_input_tokens` reflecting prompt-cache hits. See [`../crates/origin-cost.md`](../crates/origin-cost.md). |

---

## Themed groupings

### Storage & data

The CAS is the spine of origin's storage: blobs are keyed by **content hash**,
chunked with **FastCDC** for deduplication, and referenced by **CAS handle**
rather than copied around. Session and graph state are serialized as **IR**
using `rkyv`; the **archived IR** form is memory-mapped and read with zero
deserialization, which is what makes warm starts fast. The **Hot / Warm / Cold**
tiers express the latency gradient: live in-memory state, mmap'd archived IR, and
on-disk/CAS state reconstructed on demand. See
[`../architecture/data-and-storage.md`](../architecture/data-and-storage.md).

### Runtime & scheduling

origin runs a **two-runtime model** so latency-sensitive IPC/UI work cannot be
starved by CPU-heavy agent or code-graph work. `spawn_in` chooses the executor
for a task, and a **task class** sets its priority — resumed tool calls
re-spawn under `TaskClass::Critical`. See
[`../architecture/runtime-and-concurrency.md`](../architecture/runtime-and-concurrency.md).

### Agency & orchestration

A **skill** masks the tool set (**allowed-tools**) and primes the prompt; a
**workflow** chains skills into dependency layers. The **swarm** is the pool of
confined **sub-agents** dispatched by `Task`/`RunWorkflow`, each returning a
**CompletionReport**. The **goal driver** iterates a long-running goal; **ambient**
work runs during idle time; **steering** lets an operator nudge a running agent.
See [`../subsystems/swarm-and-orchestration.md`](../subsystems/swarm-and-orchestration.md).

### Safety & control

Tools carry a **permission tier** and run children under a **sandbox profile**.
Secrets are wrapped in **Secret\<T\>** and stored in the **keyvault**.
Non-interactive runs are bounded by **policy/governance**. Remote access uses
**pairing** and **certificate-fingerprint pinning**. See
[`../security/security-model.md`](../security/security-model.md).

### Protocol

Clients and the daemon exchange **frames** tagged by **FrameKind**; requests are
**ClientMessage**s and mid-turn updates are **StreamEvent**s. See
[ipc-protocol.md](ipc-protocol.md).

---

## Acronym quick-reference

| Acronym | Expansion | Where |
|---------|-----------|-------|
| CAS | Content-Addressed Store | [`../crates/origin-cas.md`](../crates/origin-cas.md) |
| IR | Intermediate Representation (rkyv) | [`../architecture/data-and-storage.md`](../architecture/data-and-storage.md) |
| FastCDC | Fast Content-Defined Chunking | [`../crates/origin-cas.md`](../crates/origin-cas.md) |
| HNSW | Hierarchical Navigable Small World (ANN index) | [`../crates/origin-mem.md`](../crates/origin-mem.md) |
| MCP | Model Context Protocol | [`../crates/origin-mcp.md`](../crates/origin-mcp.md) |
| IPC | Inter-Process Communication | [ipc-protocol.md](ipc-protocol.md) |
| mTLS | Mutual Transport Layer Security | [ipc-protocol.md](ipc-protocol.md) |
| QUIC | UDP-based multiplexed transport | [ipc-protocol.md](ipc-protocol.md) |
| LSP | Language Server Protocol | [`../crates/origin-lsp-client.md`](../crates/origin-lsp-client.md) |
| ANN | Approximate Nearest Neighbour | [`../subsystems/memory-and-codegraph.md`](../subsystems/memory-and-codegraph.md) |
| OTLP | OpenTelemetry Protocol | [`../crates/origin-telemetry.md`](../crates/origin-telemetry.md) |
| PQ | Post-Quantum (crypto) | [ipc-protocol.md](ipc-protocol.md) |

## How the pieces relate

```
 CLI / TUI  ──frames(IPC)──►  origin-daemon  ◄──restart/replay──  supervisor
                               │  agent loop
                               ├─► router ──► provider (Anthropic/Gemini/…)
                               │            └─► sidecar (cheap NL model)
                               ├─► tools (builtin + MCP DynTool)
                               │     └─ permission tier + sandbox profile
                               ├─► swarm ──► sub-agents ──► CompletionReport
                               └─► storage: CAS (FastCDC) + archived IR + code graph
```

- A **prompt** enters as a `ClientMessage::Prompt`; the daemon streams
  `StreamEvent`s back and ends with a `ServerMessage` reply.
- **Tools** run under a **permission tier** and **sandbox profile**; results may
  be **CAS handles** inflated with `Recall`.
- **Skills/workflows** shape which tools are available; the **swarm** parallelizes
  work; the **goal driver** and **ambient** loop give autonomy.
- The **supervisor** restarts the daemon and replays **resume tokens**;
  **cassettes** enable deterministic replay.

---

_Last reviewed against workspace version 0.9.8._
