# origin-selfdev

> Binary self-development: a safe-by-construction supervised hot-reload state machine with injected side effects, build queue, and reload-context resume.

## Purpose

`origin-selfdev` orchestrates the highest-risk capability in the project:
`origin` proposing and applying modifications to its *own* source, rebuilding,
and restarting into the new binary while preserving live sessions. To make that
safe it encodes the whole edit→build→test→restart cycle as a **pure, fully
unit-tested state machine** with every dangerous side effect (source edit,
`cargo build`/`cargo test`, process restart, checkpoint/rollback) hidden behind
an injected trait. Nothing here rebuilds, restarts, or writes the user's tree on
its own — the daemon supplies the implementations and tests supply fakes.

## Public API surface

| Item | Kind | Description |
|------|------|-------------|
| `SelfDevDriver` | struct | The state machine over a job queue with injected effects. |
| `SelfDevState` | enum | `Idle → Editing → Building → Testing → AwaitingRestart → Resuming` (+ `Failed`/`RolledBack`). |
| `SelfDevEvent` | enum | `StartJob` / `EditDone` / `BuildResult` / `TestResult` / `RestartGranted` / `Resumed`. |
| `SelfDevConfig` | struct | Storm-guard tuning (`max_consecutive_failures`). |
| `SelfDevError` | enum | Invalid transition, queue-empty, storm guard, restart denied, rollback/persist failed. |
| `BuildRunner` / `TestRunner` / `Rollback` / `RestartAuthority` | trait | Injected effects. |
| `CargoRunner` | struct | Real default `BuildRunner`/`TestRunner` shelling `cargo`. |
| `BuildJob` / `BuildQueue` | struct | FIFO of self-modification jobs, one in flight. |
| `ReloadContext` / `ReloadStore` / `FileReloadStore` | struct/trait | Restart-spanning resume state. |
| `RelaunchManifest` / `RelaunchRequest` / `RelaunchStore` | struct/trait | Binary-swap contract for the supervisor. |
| `SELFDEV_RELAUNCH_EXIT_CODE` | const | Sentinel exit code the daemon emits to request a swap. |

## Key types

```rust
pub enum SelfDevState { Idle, Editing, Building, Testing, AwaitingRestart, Resuming,
                        Failed(String), RolledBack }

pub enum SelfDevEvent { StartJob, EditDone, BuildResult { ok: bool }, TestResult { ok: bool },
                        RestartGranted, Resumed }

pub trait RestartAuthority { fn authorize(&self, ctx: &ReloadContext) -> RestartDecision; }
pub enum RestartDecision { Grant, Deny(String) }

pub struct ReloadContext {
    pub session_ids: Vec<String>,
    pub pending_goal: Option<String>,
    pub in_flight_job_id: String,
    pub generation: u64,            // monotonic successor counter, survives exec
}
```

## How it works

The four safety invariants are encoded directly in the transition table so they
cannot be skipped:

```
        StartJob        EditDone       BuildResult{ok:true}   TestResult{ok:true}   RestartGranted   Resumed
 Idle ───────────► Editing ───────► Building ─────────────► Testing ────────────► AwaitingRestart ─► Resuming ─► Idle
                                       │ ok:false              │ ok:false                 ▲ (gated by RestartAuthority)
                                       ▼                       ▼                          │
                                   Failed(reason) ──► RolledBack ──► Idle      (machine waits here indefinitely
                                       (invokes injected Rollback)               until something OUTSIDE grants it)
```

1. **No restart without a green build AND a green test** — `AwaitingRestart` is
   reachable only from `Testing` on a pass that followed a passing build.
2. **Any build/test failure rolls back to the same binary** via
   `Failed → RolledBack → Idle`, invoking the injected `Rollback`.
3. **Restart is explicitly granted, never automatic** — gated by
   `RestartAuthority`; the machine sits in `AwaitingRestart` until granted.
4. **Storm guard** — consecutive failed generations are counted; at the cap the
   driver refuses to start further jobs (`StormGuardTripped`) until reset.

On a granted restart the daemon persists a `ReloadContext` (open sessions,
pending goal, in-flight job, generation) and writes a `RelaunchManifest`, then
exits with `SELFDEV_RELAUNCH_EXIT_CODE`; the **supervisor** (which does not
depend on this crate, only re-exporting the sentinel) execs the new binary.

## Dependencies & features

- Runtime deps: `serde`/`serde_json` (context + manifest persistence),
  `thiserror`. A pure leaf crate — no daemon/supervisor edges — so the
  supervisor can share the sentinel without a dependency cycle. The `cargo`
  module's `CargoRunner` shells real `cargo` but is opt-in (the daemon chooses
  it; tests never touch it).
- Dev-deps: `tempfile`. `#![forbid(unsafe_code)]`. No Cargo features.

## Used by

`Grep "origin-selfdev" glob "crates/*/Cargo.toml"` →

- `crates/origin-daemon/Cargo.toml`
- `crates/origin-selfdev/Cargo.toml` (self)
- `crates/origin-supervisor/Cargo.toml`

The daemon's `selfdev` module drives the machine with real effects; the
supervisor consumes only `SELFDEV_RELAUNCH_EXIT_CODE` and the manifest contract.

## Testing

In-file `#[cfg(test)]` unit tests cover the full transition graph (every legal
edge, every rejected event, rollback paths, storm-guard trip/reset, restart
authority grant/deny) with fake effect traits, plus `FileReloadStore` round-trips
(dev-dep `tempfile`). A cross-crate sync test in `origin-supervisor`
(`relaunch_sentinel_matches_canonical`) guards the exit-code against drift.

## See also

- [Swarm & orchestration subsystem](../subsystems/swarm-and-orchestration.md)
- [Runtime & concurrency architecture](../architecture/runtime-and-concurrency.md)
- [origin-supervisor](./origin-supervisor.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
