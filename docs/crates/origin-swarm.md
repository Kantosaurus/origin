# origin-swarm

> Coordinator/worker protocol with completion reports.

## Purpose

`origin-swarm` is the coordinator/worker substrate that lets the agent fan out
into concurrent sub-agents and aggregate their results. A `Coordinator` admits,
spawns, and awaits workers; every worker authors against one shared, mutex-guarded
[plan fold](./origin-plan.md) and hands back a structured `CompletionReport` —
no prose, just plan ops, files touched, decisions, follow-ups, a transcript CAS
handle, and usage accounting. Memory-governed admission control parallelises as
far as RAM allows, and named-team primitives let a higher layer talk about
workers as a persistent team.

## Public API surface

| Item | Kind | Description |
|------|------|-------------|
| `Coordinator` | struct | Owns workers, dispatches them, aggregates reports. |
| `WorkerHandle` / `WorkerId` | struct | Opaque handles for spawned workers. |
| `CompletionReport` | struct | Structured worker → coordinator handoff. |
| `WorkerSpec` | struct | Input to `Coordinator::spawn` (goal, tools, budget, MCP). |
| `Budget` / `Usage` / `ReportStatus` / `DecisionRecord` / `TaskRef` | struct/enum | Spec & report records. |
| `PlanHandle` | struct | Shared mutex-guarded plan fold workers author against. |
| `AdmissionGate` / `AdmissionTicket` / `MemoryProbe` | struct/trait | Memory-governed admission. |
| `Team` / `Teammate` / `TeamRegistry` / `MissionLog` | struct | Named-team vocabulary. |
| `FileRegistry` / `Mailbox` / `Message` | struct | Real-time collab primitives (WS-L). |
| `WorkerFn` / `WorkerContext` / `default_noop_worker` | type/struct/fn | The pluggable worker closure. |
| `SwarmError` | enum | Crate error type. |
| `collab_enabled` | fn | Whether `ORIGIN_SWARM_COLLAB` real-time collab is active. |

Module map: `admission`, `collab`, `coordinator`, `error`, `lifecycle`, `report`, `rpc`, `spec`, `team`, `worker`.

## Key types

```rust
pub struct CompletionReport {
    pub goal: String,
    pub status: ReportStatus,
    pub plan_updates: Vec<origin_plan::OpEnvelope>,   // ops authored against the shared plan
    pub files_touched: Vec<[u8; 32]>,                 // CAS handles
    pub decisions: Vec<DecisionRecord>,
    pub follow_ups: Vec<TaskRef>,
    pub transcript_handle: [u8; 32],                  // CAS handle of full chat log
    pub usage: Usage,
    pub detail: Option<String>,
}
impl CompletionReport { pub fn store_in_cas(&self, cas: &CasStore) -> Result<[u8; 32], _>; }

pub struct WorkerSpec { pub goal: String, pub allowed_tools: Vec<String>, /* budget, mcp, ... */ }
pub struct Budget { pub max_wall_ms: u64, pub max_input_tokens: u64,
                    pub max_output_tokens: u64, pub max_tool_calls: u32 }
```

## How it works

```
WorkerSpec ─► AdmissionGate.admit().await ─► AdmissionTicket (RAII)
                  │  min(static ceiling, live memory governor); >=1 forward floor
                  ▼
        Coordinator.spawn() ─► spawn_in(TaskClass::Swarm, WorkerFn)
                  │                 each worker authors via PlanHandle (shared fold)
                  │                 + optional WorkerCollab room (FileRegistry + Mailbox)
                  ▼
        await_completion() ◄── CompletionReport (stored in CAS, handle returned)
```

The **report carries no prose by design**: free-form text lives in the
transcript (referenced by `transcript_handle`), so the inlining parent can
parse the structured handoff without an LLM round-trip. `store_in_cas`
bincode-encodes the report so completion fan-out stays cheap.

**Admission control** is the binding limiter: an injected `MemoryProbe` reports
available RAM and each admitted worker debits a committed per-worker reserve, so
a burst of concurrent admits can't overshoot. Backpressure is *await, never
reject*; a parked admit resumes when an in-flight worker's RAII ticket drops.
The default fallback is **unlimited** (`u32::MAX`) — set `ORIGIN_SWARM_MAX=<n>`
to cap. Real-time collaboration (`ORIGIN_SWARM_COLLAB`, default-on) gives every
worker a shared room so concurrent workers notify each other of file shifts.

The default worker is `default_noop_worker`; the daemon substitutes the real
agent-loop `WorkerFn` via `Coordinator::set_default_worker`.

## Dependencies & features

- Runtime deps: `origin-core`, `origin-plan`, `origin-cas`, `origin-store`,
  `origin-planner`, `origin-runtime` (`spawn_in`/`TaskClass`), `tokio`
  (sync/time/rt), `bincode`, `serde`, `thiserror`, `ulid`.
- Dev-deps: `tempfile`. No Cargo features.

## Used by

`Grep "origin-swarm" glob "crates/*/Cargo.toml"` →

- `crates/origin-daemon/Cargo.toml`
- `crates/origin-swarm/Cargo.toml` (self)
- `crates/origin-tools/Cargo.toml`

`origin-tools` exposes the `Task` (dispatch sub-agent) tool; the daemon's
`swarm_worker` and `teams` modules wire the real worker closure and team
bookkeeping.

## Testing

`tests/` directory: `protocol.rs` (the P9.6 noop-worker protocol round-trip),
`concurrency.rs` (concurrent dispatch + plan-handle authoring), and
`admission.rs` (scripted `MemoryProbe` readings driving the gate). Backed by
in-file unit tests throughout.

## See also

- [Swarm & orchestration subsystem](../subsystems/swarm-and-orchestration.md)
- [Agent & sessions subsystem](../subsystems/agent-and-sessions.md)
- [origin-plan](./origin-plan.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
