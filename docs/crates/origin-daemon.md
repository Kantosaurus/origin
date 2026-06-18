# origin-daemon

> Daemon hosting origin session, agent, and protocol services.

## Purpose

`origin-daemon` is the long-running host process for the agent. It owns the
agent loop, session lifecycle and persistence, the IPC protocol clients talk to,
provider construction, and the wiring that bridges the supporting orchestration
crates (goal, swarm, plan, sidecar, ambient, schedule, selfdev, …) into one
running service. This page is a **module-level tour** of `lib.rs` — the crate is
large, so rather than cover every line it maps each public module to its
responsibility. The deep mechanics of individual subsystems live in their own
crate pages and the subsystem docs.

## Module map

| Module | Responsibility |
|--------|----------------|
| `agent` | Agent loop: prompt → provider → tool dispatch → repeat → final text. |
| `anthropic_verifier` | Concrete goal `Verifier` backed by a `Provider` (Haiku-class). |
| `auth` | Bearer-token authorization store for remote IPC (P13.2). |
| `compactor` | Summary-backed transcript compaction (P5.4). |
| `config` | Daemon-wide configuration knobs sourced from env vars. |
| `daemon_memory_handle` | Re-exports the daemon's concrete `MemoryHandle`. |
| `default_workflow` | Default-workflow directive prepended to every system prompt. |
| `goal_checkpoint` | Goal-aware `ResumeToken` checkpoint writer. |
| `goal_clear_all` | Couples `/clear` to clearing the active goal (bug #10). |
| `goal_driver` | After each `run_loop`, decides whether to verify/iterate/clear the goal. |
| `ipc_prompter` | Opt-in interactive permission prompting over IPC. |
| `lsp_diagnostics` | Autonomous post-edit LSP diagnostics feedback. |
| `memory_wiring` | Memory subsystem wiring (P6.9); exports `MemoryWiring` / `MemoryDispatchHandle`. |
| `model_default` | Resolves the default model for daemon-spawned sub-agent loops. |
| `model_window` | The single shared per-model context-window resolver. |
| `pairing` | Pairing state machine for remote-IPC bring-up (P13). |
| `plan_bus` | Daemon-wide plan-op broadcast bus (P9.9 → P10). |
| `proposal_registry` | Daemon-wide registry of pending memory proposals. |
| `protocol` | IPC request/response shapes for daemon ↔ client. |
| `provider_factory` | Builds an `Arc<dyn Provider>` for a given model/account. |
| `ra_impl` | `DiagnosticsHandle` wrapping `origin-lsp-client::LspClient`. |
| `remote_quic` | Remote QUIC transport wiring (R7). |
| `runtime_launch` | Two-runtime launcher — control core + worker pool. |
| `session` | `Session` — in-memory message log + metadata. |
| `session_store` | SQLite-backed session persistence. |
| `shutdown` | Phased cooperative shutdown (N8.10). |
| `skill_catalog` | In-process catalog of skills loaded from `~/.origin/skills/`. |
| `stream_relay` | Bridges a per-request `Ring` to an IPC connection as JSON. |
| `tool_use_parser` | Incremental SAX-style JSON parser for `tool_use` inputs (N2.2). |
| `workflow_progress` | Step-by-step workflow execution state. |
| `workflow_runner` | Phase-layered **parallel** workflow execution. |
| `workflows` | Daemon-side `workflows.toml` loader. |
| `ambient` | Default-off ambient (always-on) autonomy tick loop. |
| `hooks_runtime` | Process-wide hooks runtime: shell pools fired at agent-loop points. |
| `mem_garden` | Default-off idle-time auto-memory mining loop. |
| `overnight` | Default-off overnight autonomous driver. |
| `routing` | Live, per-turn model routing wired onto the agent loop. |
| `scheduler` | Default-off background scheduler tick loop. |
| `selfdev` | Daemon-side control plane for binary self-development. |
| `subagents_md` | Markdown-defined, tool-isolated subagents. |
| `subsystems` | Additive, default-off bridges into the Wave-1/2 subsystem crates. |
| `supervisor` | Daemon-side wiring for the pure `origin_supervisor::LifecyclePolicy`. |
| `swarm_worker` | Real swarm worker: runs the agent loop for a child sub-agent. |
| `teams` | Daemon-side control plane for named agent teams. |
| `webhook` | Default-off authenticated webhook listener (Triggers). |

## Key types

```rust
// lib.rs re-export — the memory subsystem entry point.
pub use memory_wiring::{MemoryDispatchHandle, MemoryWiring};
```

The agent loop (`agent`), `protocol` IPC shapes, and `session`/`session_store`
are the structural core; orchestration is layered on top via dedicated modules
(`goal_driver`, `swarm_worker`/`teams`, `plan_bus`, `selfdev`, `scheduler`,
`ambient`/`overnight`, `webhook`). The autonomy loops (`ambient`, `overnight`,
`scheduler`, `mem_garden`, `webhook`) are all **default-off** and tick over the
pure-policy crates documented in this group.

## How it works

```
                       ┌──────────────────────── origin-supervisor (parent) ────────────────────────┐
                       │  spawns / restarts / binary-swaps the daemon, sheds sessions under pressure │
                       └───────────────────────────────────┬────────────────────────────────────────┘
 client (CLI/TUI) ──IPC(protocol)──► daemon                ▼
                       │   session / session_store ──► agent loop ──► provider_factory ──► Provider
                       │        │                          │  tool dispatch
                       │        │                          ├─ goal_driver  (origin-goal)
                       │        │                          ├─ swarm_worker / teams (origin-swarm + origin-plan, plan_bus)
                       │        │                          ├─ scheduler / ambient / overnight (origin-schedule/-ambient)
                       │        │                          └─ selfdev (origin-selfdev) ──► relaunch request ──► supervisor
                       │   sidecar (origin-sidecar): summaries / extraction off the hot path
```

The daemon binds the pure orchestration crates to real effects: it supplies the
wall clock and tokio timers the schedule/ambient/overnight policies lack, the
build/test/rollback/restart implementations the self-dev state machine demands,
the real worker closure the swarm coordinator dispatches, and the providers the
goal verifier and agent loop call. Two tokio runtimes (`runtime_launch`) split a
control core from a worker pool.

## Dependencies & features

- Depends on essentially every workspace crate — including all of this group's
  orchestration crates (`origin-goal`, `origin-swarm`, `origin-plan`,
  `origin-planner`, `origin-sidecar`, `origin-ambient`, `origin-schedule`,
  `origin-selfdev`, `origin-supervisor`, `origin-workflowgen`) plus providers,
  tools, storage, memory, and IPC.
- Features: `default = ["openai", "gemini", "ollama", "openrouter",
  "openai-compat", "oauth-providers", "custom-providers"]`; optional `bedrock`,
  `github-models`, and `otel` (OpenTelemetry OTLP metrics export, off by default).
  `openai`/`openrouter` are aliases for the generic `openai-compat` path.
- Ships the `origin-daemon` binary plus `phase2_ram_soak` and `long_session`
  benchmarks.

## Used by

`Grep "origin-daemon" glob "crates/*/Cargo.toml"` →

- `crates/origin-cli/Cargo.toml`
- `crates/origin-daemon/Cargo.toml` (self)
- `crates/origin-supervisor/Cargo.toml`

The CLI drives the daemon over IPC; the supervisor launches and manages the
daemon process. (The daemon itself is documented in depth elsewhere — this page
is the orchestration-focused module tour.)

## Testing

A large `tests/` directory (60+ integration tests). Orchestration-relevant
suites include `swarm_parallel_dispatch.rs`, `swarm_worker_e2e.rs`,
`workflow_runner_parallel.rs`, `plan_bus.rs`, the `goal_*` family (activation,
checkpoint, max-iter/budget caps, verifier rejection/resume, interrupts),
`sidecar_summary.rs`/`sidecar_extract.rs`, `shutdown_phases*.rs`,
`session_persist.rs`/`resume_session.rs`, and `loop_e2e.rs`. A `fuzz/` target
exercises the tool-use parser.

## See also

- [Agent & sessions subsystem](../subsystems/agent-and-sessions.md)
- [Swarm & orchestration subsystem](../subsystems/swarm-and-orchestration.md)
- [Runtime & concurrency architecture](../architecture/runtime-and-concurrency.md)
- [origin-supervisor](./origin-supervisor.md) · [origin-goal](./origin-goal.md) · [origin-swarm](./origin-swarm.md) · [origin-selfdev](./origin-selfdev.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
