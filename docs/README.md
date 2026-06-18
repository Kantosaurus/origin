# origin documentation

**`origin`** is a Rust-native agentic coding harness: a thin CLI/TUI client plus a
supervised daemon that runs LLM-driven coding sessions locally. It runs on the
same shape of **model + tools + skills + hooks + permissions** as other coding
agents, but treats four performance KPIs — cold start, keystroke-to-pixel latency,
steady RSS, and cache hit rate — as first-class CI gates, and gives every
signature subsystem an original mechanism.

This handbook is the engineering reference for the workspace: **~77 crates**,
a daemon/CLI split that communicates only over `origin-ipc`, content-addressed
storage, archived `rkyv` IR end-to-end, ~40 providers behind one catalog, per-OS
sandboxing, and deterministic replay.

> **Scope & accuracy.** These pages are grounded in the source as of workspace
> version **`0.9.8`**. Each page carries a "Last reviewed against workspace
> version" line. Where a detail could not be verified in code it is marked
> *inferred*. When the code changes, update the affected page and bump its
> review line.

---

## Start here

| If you want to… | Read |
| --- | --- |
| Install and run origin for the first time | [Getting started](guides/getting-started.md) |
| Understand the whole system in one sitting | [Architecture overview](architecture/overview.md) |
| Configure origin | [Configuration](guides/configuration.md) |
| Connect a model provider | [Provider setup](guides/providers-setup.md) |
| Write your own skill | [Authoring skills](guides/authoring-skills.md) |
| Contribute code | [Contributing](development/contributing.md) |
| Look up a term | [Glossary](reference/glossary.md) |

---

## Table of contents

### Architecture

The big-picture design: how the pieces fit, how requests flow, how concurrency
and storage work.

- [Overview](architecture/overview.md) — system at a glance, daemon/CLI split, request lifecycle, the crate map.
- [Runtime & concurrency](architecture/runtime-and-concurrency.md) — the two-runtime daemon, task classes & `spawn_in`, backpressure, allocator strategy, shutdown, the perf-as-gate KPIs.
- [Data & storage](architecture/data-and-storage.md) — content-addressed storage, the Hot/Warm/Cold tiers, FastCDC, archived IR persistence, the relational store, session resume.

### Subsystems

Domain deep-dives. Each maps to one or more crates and links to their reference pages.

- [Agent loop & sessions](subsystems/agent-and-sessions.md) — the session lifecycle, the agent control loop, compaction, the goal driver, verification, steering, spend caps.
- [Providers](subsystems/providers.md) — the `Provider` trait, the wire drivers, the openai-compat driver + shimquirks, model discovery, routing, cost.
- [Tools](subsystems/tools.md) — the tool registry & trait, the builtins, permission tiering, output compaction & CAS handles, edit formats, MCP, web/browser, LSP, VCS safety.
- [Skills, hooks & workflows](subsystems/skills.md) — the skill model, loading & precedence, the embedded catalog, embedding-indexed lazy injection, `allowed-tools` narrowing, hooks, workflows.
- [Memory, code-graph & retrieval](subsystems/memory-and-codegraph.md) — conversation memory (MiniLM + HNSW + temporal decay), gardening, the code knowledge graph, graph queries & tools, the repo map, the knowledge index.
- [Swarm & orchestration](subsystems/swarm-and-orchestration.md) — the coordinator/worker protocol, the CRDT shared plan, dependency-layer fan-out, ambient/overnight autonomy, scheduling, the agent-facing Task/Workflow API.
- [TUI & CLI](subsystems/tui-and-cli.md) — the cell-grid renderer & damage diffing, syntax/markdown, the command surface, the interactive session UI, slash commands, output styles, i18n, onboarding.
- [Observability, telemetry & diagnostics](subsystems/observability.md) — developer tracing (parquet ring + OTLP), operational metrics (Prometheus), opt-in product telemetry, `origin doctor`, cost accounting, notifications.

### Security

- [Security model](security/security-model.md) — threat model, permission tiers, `allowed-tools` narrowing, command-line safety, per-OS sandboxing, secret handling, governance policy, remote-transport security, the zero-`unsafe` posture, an operator hardening checklist.
- [`unsafe` audit](security/unsafe-audit.md) — the audited crates permitted to use `unsafe`, and why.
- [P14 security review signoff](security/p14-security-review.md) — the sandbox/KeyVault review checklist.

### Guides

Task-oriented walkthroughs for users.

- [Getting started](guides/getting-started.md)
- [Configuration](guides/configuration.md)
- [Authoring skills](guides/authoring-skills.md)
- [Migration from other harnesses](guides/migration.md)
- [Provider setup](guides/providers-setup.md)

### Operations

For running and operating origin.

- [Deployment](operations/deployment.md) — install channels, auto-update, the single-binary model, remote daemon.
- [Daemon & supervisor](operations/daemon-and-supervisor.md) — lifecycle, restart/resume, shutdown/draining, the IPC socket.
- [Observability runbook](operations/observability-runbook.md) — enabling OTLP, scraping Prometheus, reading traces, telemetry on/off, diagnostics.
- [CI automation](operations/ci-automation.md) — the `@origin` bot, PR review, issue triage, scheduled maintenance, and the quality-gate workflows.
- [Benchmarking](operations/benchmarking.md) — what `origin-bench` measures and the CI perf gate.
- [Troubleshooting](operations/troubleshooting.md) — symptom → cause → fix.

### Development

For contributors.

- [Contributing](development/contributing.md)
- [Building & testing](development/building-and-testing.md)
- [Coding standards](development/coding-standards.md)
- [Workspace layout](development/workspace-layout.md)
- [Adding a crate](development/adding-a-crate.md)
- [Release process](development/release-process.md)

### Reference

Look-up material.

- [Crate index](crates/README.md) — every workspace crate, grouped by layer.
- [Glossary](reference/glossary.md)
- [Tool catalog](reference/tool-catalog.md)
- [Environment variables](reference/environment-variables.md)
- [IPC protocol](reference/ipc-protocol.md)
- [FAQ](reference/faq.md)

---

## How this documentation is organized

```text
docs/
├── README.md                     ← you are here (entry point + TOC)
├── architecture/                 system design & cross-cutting concerns
├── subsystems/                   per-domain deep-dives
├── security/                     the security model
├── guides/                       task-oriented user walkthroughs
├── operations/                   deploy / run / observe / troubleshoot
├── development/                  contributor handbook
├── reference/                    glossary, catalogs, protocol, FAQ
└── crates/                       one reference page per workspace crate
```

**Conventions.** Diagrams are Mermaid or ASCII. Code snippets are copied from the
source (not paraphrased). Cross-references are relative links so the tree renders
correctly on GitHub and in an mdBook build alike.

---

## Project links

- Source: the workspace root (`Cargo.toml`, `crates/`, `xtask/`).
- Top-level docs: [`CONTRIBUTING.md`](../CONTRIBUTING.md), [`SECURITY.md`](../SECURITY.md), [`GOVERNANCE.md`](../GOVERNANCE.md), [`ROADMAP.md`](../ROADMAP.md), [`CHANGELOG.md`](../CHANGELOG.md).
- License: **Apache-2.0** (see [`LICENSE`](../LICENSE) and [`NOTICE`](../NOTICE)).

_Last reviewed against workspace version 0.9.8._
