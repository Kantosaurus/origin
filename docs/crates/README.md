# Crate index

The `origin` workspace is a Cargo workspace of **~77 crates** (`crates/*` plus
`xtask`; `crates/origin-daemon/fuzz` is excluded). Each crate below links to its
reference page and is grouped by architectural layer. One-line roles are the
crate's own `Cargo.toml` `description`.

For the big-picture relationships between these layers, see the
[Architecture overview](../architecture/overview.md). For the contributor view of
the tree, see [Workspace layout](../development/workspace-layout.md).

> **77 crates, one rule:** every crate inherits `[lints] workspace = true`, so
> `unsafe` is forbidden (audited exceptions in `cas`/`tui`/`ipc`), clippy
> `pedantic` + `nursery` are on, and `unwrap_used` is denied.

---

## Core & IR

The load-bearing types and the canonical intermediate representation.

| Crate | Role |
| --- | --- |
| [`origin-core`](origin-core.md) | Core message types, IR, and provider capabilities for origin. |

## Transport & IPC

The only channel between the CLI and the daemon.

| Crate | Role |
| --- | --- |
| [`origin-ipc`](origin-ipc.md) | Framed local-socket and QUIC/TLS IPC transports for origin. |
| [`origin-resume-token`](origin-resume-token.md) | MAC-authenticated session resume token shared by the daemon and supervisor. |

## Storage, CAS & persistence

Content-addressed storage, the relational store, ring buffers, and replay.

| Crate | Role |
| --- | --- |
| [`origin-cas`](origin-cas.md) | Content-addressed store with FastCDC chunking, mmap pack files, and three-tier storage. |
| [`origin-store`](origin-store.md) | SQLite-backed store for origin with embedded refinery migrations. |
| [`origin-stream`](origin-stream.md) | Single-producer multi-consumer byte ring with per-subscriber read cursors. |
| [`origin-replay`](origin-replay.md) | Deterministic record-and-replay for origin sessions. |
| [`origin-cassette`](origin-cassette.md) | Deterministic, secret-safe HTTP cassette recording and sequential replay matching. |

*See: [Data & storage](../architecture/data-and-storage.md).*

## Runtime & allocation

Concurrency budgeting and memory arenas.

| Crate | Role |
| --- | --- |
| [`origin-runtime`](origin-runtime.md) | Task-class budgeting with per-class semaphores and a `spawn_in` helper. |
| [`origin-alloc`](origin-alloc.md) | Per-component allocator arenas with a no-op default and opt-in jemalloc backend. |

*See: [Runtime & concurrency](../architecture/runtime-and-concurrency.md).*

## Providers & model routing

The provider trait, the wire drivers, model discovery, routing, and cost.

| Crate | Role |
| --- | --- |
| [`origin-provider`](origin-provider.md) | Provider trait with canonical request/response/usage/error types for origin. |
| [`origin-provider-anthropic`](origin-provider-anthropic.md) | Anthropic Messages API provider for origin. |
| [`origin-provider-openai-compat`](origin-provider-openai-compat.md) | Generic OpenAI Chat-Completions wire client for OpenAI-shape providers. |
| [`origin-provider-gemini`](origin-provider-gemini.md) | Google Gemini generateContent provider for origin, with SSE streaming. |
| [`origin-provider-bedrock`](origin-provider-bedrock.md) | SigV4-signed AWS Bedrock provider for origin (Anthropic InvokeModel). |
| [`origin-provider-ollama`](origin-provider-ollama.md) | Ollama provider for origin with NDJSON streaming chat. |
| [`origin-provider-github`](origin-provider-github.md) | GitHub Copilot provider for origin with vault-sourced OAuth session tokens. |
| [`origin-shimquirks`](origin-shimquirks.md) | OpenAI-compatibility shim per-backend request/response quirk handling. |
| [`origin-modeldiscovery`](origin-modeldiscovery.md) | Runtime model discovery: parse provider model listings, merge with builtin catalog, cache with a TTL. |
| [`origin-router`](origin-router.md) | Model routing strategies (architect/editor split, phase-aware, scored, quota fallback). |
| [`origin-cost`](origin-cost.md) | Per-turn and cumulative USD cost + token accounting with prompt-cache economy awareness. |
| [`origin-oidc`](origin-oidc.md) | Workload Identity Federation / OIDC token-exchange for keyless CI auth. |

*See: [Providers](../subsystems/providers.md).*

## Agent, daemon & orchestration

The session host and the autonomy/orchestration crates around it.

| Crate | Role |
| --- | --- |
| [`origin-daemon`](origin-daemon.md) | Daemon hosting origin session, agent, and protocol services. |
| [`origin-supervisor`](origin-supervisor.md) | Restarts origin-daemon on crash and resumes sessions across restarts. |
| [`origin-swarm`](origin-swarm.md) | Coordinator/worker protocol with completion reports. |
| [`origin-plan`](origin-plan.md) | CRDT op-log and deterministic fold for the shared swarm plan. |
| [`origin-goal`](origin-goal.md) | Goal driver with persistent completion conditions and an inline self-tag protocol. |
| [`origin-sidecar`](origin-sidecar.md) | Always-on small-model worker with a bounded queue and pooled workers. |
| [`origin-ambient`](origin-ambient.md) | Resource-aware always-on + overnight autonomous mode policy under an adaptive token budget. |
| [`origin-schedule`](origin-schedule.md) | Pure-logic scheduling, cron/interval/daily spec parsing, and a trigger queue. |
| [`origin-steering`](origin-steering.md) | Mid-execution steering hints queued and merged into the next agent turn. |
| [`origin-selfdev`](origin-selfdev.md) | Binary self-development: a safe-by-construction supervised hot-reload state machine. |
| [`origin-workflowgen`](origin-workflowgen.md) | Deterministic offline workflow authoring: decompose a NL goal into a validated skill pipeline. |

*See: [Agent loop & sessions](../subsystems/agent-and-sessions.md), [Swarm & orchestration](../subsystems/swarm-and-orchestration.md).*

## Tools & integrations

The tool registry and every builtin/integration tool.

| Crate | Role |
| --- | --- |
| [`origin-tools`](origin-tools.md) | Tool registry, macros, and builtin tools for origin. |
| [`origin-mcp`](origin-mcp.md) | Model Context Protocol client with stdio/HTTP-SSE transports and OAuth. |
| [`origin-browser`](origin-browser.md) | Dual-backend browser router with one-shot WebFetch and Tavily WebSearch. |
| [`origin-websearch`](origin-websearch.md) | Pluggable web search with offline-testable result parsers and an injected fetcher. |
| [`origin-multimodal`](origin-multimodal.md) | Image and PDF context ingestion: classify, extract text, and build content blocks. |
| [`origin-gmail`](origin-gmail.md) | First-class Gmail tool over Google OAuth2 with token-frugal metadata reads. |
| [`origin-editfmt`](origin-editfmt.md) | Model-tuned edit-format matrix: parsers and appliers for diff formats. |
| [`origin-postedit`](origin-postedit.md) | Post-edit lint/test/format policy with a builtin formatter table (decision logic only). |
| [`origin-lsp-client`](origin-lsp-client.md) | Minimal stdio JSON-RPC Language Server client for diagnostics. |
| [`origin-lspfleet`](origin-lspfleet.md) | Registry and auto-install decisioning for 40+ language servers, plus diagnostic aggregation. |
| [`origin-vcs`](origin-vcs.md) | Agent-native git safety layer: shadow-git checkpoints, restore, rewind, lane/draft-patch model. |
| [`origin-scout`](origin-scout.md) | Read-only dependency-source research: shallow-clone planning and repo overview extraction. |
| [`origin-watch`](origin-watch.md) | Editor-agnostic watcher that scans source files for AI-trigger comments. |
| [`origin-clipboard`](origin-clipboard.md) | Copy/paste web-chat mode: format context to paste and parse pasted edits. |
| [`origin-voice`](origin-voice.md) | Speech-to-text dictation config and transcript interleave policy. |

*See: [Tools](../subsystems/tools.md).*

## Skills, memory & code-graph

Knowledge: skills, conversation memory, the code graph, and retrieval.

| Crate | Role |
| --- | --- |
| [`origin-skills`](origin-skills.md) | Skills loader with embedding upsert and allowed-tools narrowing. |
| [`origin-mem`](origin-mem.md) | Conversation memory with ONNX MiniLM embeddings, HNSW search, and temporal-decay re-ranking. |
| [`origin-codegraph`](origin-codegraph.md) | Native code knowledge graph with tree-sitter extraction and SQLite index. |
| [`origin-knowledge`](origin-knowledge.md) | Local knowledge index: full-text inverted index + cosine vector search, JSON-persistable. |
| [`origin-repomap`](origin-repomap.md) | Personalized-PageRank repo map over a symbol graph, packed into a token budget. |
| [`origin-hooks`](origin-hooks.md) | Pre-spawned shell pool and typed lifecycle event dispatch. |

*See: [Skills, hooks & workflows](../subsystems/skills.md), [Memory & code-graph](../subsystems/memory-and-codegraph.md).*

## TUI & CLI

The user-facing client and its presentation crates.

| Crate | Role |
| --- | --- |
| [`origin-cli`](origin-cli.md) | Terminal UI and CLI for the origin agent runtime. |
| [`origin-tui`](origin-tui.md) | Custom cell-grid terminal renderer with SIMD damage diffing and ANSI emit. |
| [`origin-ui-preview`](origin-ui-preview.md) | Hot-reload terminal preview of the origin harness UI/UX (themes, palette, ANSI chrome). |
| [`origin-mermaid`](origin-mermaid.md) | Dependency-free renderer for a useful subset of mermaid flowcharts to ASCII. |
| [`origin-i18n`](origin-i18n.md) | Lightweight std-only UI string catalog with locale fallback and placeholder substitution. |
| [`origin-outputstyle`](origin-outputstyle.md) | Output styles (Explanatory/Learning/Concise) plus a transform-or-hide MessageDisplay hook. |
| [`origin-planner`](origin-planner.md) | Predictive prompt-cache prefix planner with stability bands and a prefix ledger. |

*See: [TUI & CLI](../subsystems/tui-and-cli.md).*

## Security, sandbox & governance

The defense layers.

| Crate | Role |
| --- | --- |
| [`origin-permission`](origin-permission.md) | Tier-based permission engine with a pluggable prompter. |
| [`origin-sandbox`](origin-sandbox.md) | Per-tool sandbox profiles for Linux, macOS, and Windows. |
| [`origin-keyvault`](origin-keyvault.md) | Cross-platform secret storage with a uniform async facade over per-OS keystores. |
| [`origin-cmdparse`](origin-cmdparse.md) | Bash command-line safety analysis that hardens the permission gate against known bypass classes. |
| [`origin-conseca`](origin-conseca.md) | Dynamic, model-generated per-prompt security policy parsed and enforced per tool call. |
| [`origin-policy`](origin-policy.md) | Layered governance / managed-settings engine: RBAC, model allow-lists, spend caps, trusted folders. |

*See: [Security model](../security/security-model.md).*

## Observability, telemetry & ops

Signals and operational tooling.

| Crate | Role |
| --- | --- |
| [`origin-trace`](origin-trace.md) | Tracing layer that writes spans to a per-day parquet ring with queryable predicates. |
| [`origin-metrics`](origin-metrics.md) | Bounded-cardinality counters with a Prometheus text encoder. |
| [`origin-telemetry`](origin-telemetry.md) | Opt-in, self-hostable product telemetry pipeline with secret redaction and sampling. |
| [`origin-doctor`](origin-doctor.md) | Environment/runtime diagnostics with injected probes plus a privacy phone-home disclosure. |
| [`origin-notify`](origin-notify.md) | Out-of-band human notifications with quiet-hours, batching policy, and injectable channel dispatch. |

*See: [Observability](../subsystems/observability.md).*

## Migration, review & utilities

Cross-harness migration, review aggregation, export, and benchmarking.

| Crate | Role |
| --- | --- |
| [`origin-migrate`](origin-migrate.md) | Migrate sessions, skills, and memories from other harnesses into origin. |
| [`origin-review`](origin-review.md) | Multi-agent confidence-scored review aggregation + issue auto-triage. |
| [`origin-export`](origin-export.md) | Conversation transcript export to clean Markdown or pretty JSON. |
| [`origin-plugin`](origin-plugin.md) | Plugin packaging, manifest parsing, dependency resolution, and live cross-tool skill discovery. |
| [`origin-bench`](origin-bench.md) | Benchmark harness comparing origin against other coding-agent CLIs (`publish = false`). |

*See: [Migration guide](../guides/migration.md), [Benchmarking](../operations/benchmarking.md).*

---

[← Back to the documentation home](../README.md)

_Last reviewed against workspace version 0.9.8._
