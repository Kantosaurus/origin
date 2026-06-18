# origin-runtime

> Task-class budgeting with per-class semaphores and a spawn_in helper

## Purpose

`origin-runtime` is the daemon's concurrency governor. Every async task in the
daemon is spawned through `spawn_in(class, fut)`, which acquires a per-class
semaphore permit before polling the future. This buckets work by priority
(agent turns vs. background GC vs. swarm workers) and enforces a fairness rule:
low-priority `Bulk` work is parked whenever any `Critical` task is in flight.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `TaskClass` | enum | The priority/budget bucket for a task (6 variants). |
| `spawn_in` | fn | Spawn a future under a class; acquires its permit first. |
| `BulkGate` | struct | Watcher handle: `wait_until_idle`, `notify_all`, `current`. |
| `critical_tasks_in_flight` | fn | Count of `Critical` tasks currently holding a permit. |
| `init_for_test` | fn | Idempotently materialise the process-singleton registry. |

## Key types

```rust
#[repr(u8)]
pub enum TaskClass {
    Critical = 0,   // agent turns; provider HTTP/2; tool exec; swarm bodies
    Realtime = 1,   // renderer ticks; IPC dispatch; per-stream relays
    Sidecar  = 2,   // small-model jobs; MCP clients; hook dispatch
    Background = 3, // CAS GC; SQLite vacuum; idle memory consolidation
    Bulk     = 4,   // initial code-graph build; bulk discovery (parked by Critical)
    Swarm    = 5,   // swarm sub-agent worker bodies (isolated pool)
}
impl TaskClass { pub const COUNT: usize = 6; pub const fn label(self) -> &'static str; }
```

```rust
pub fn spawn_in<F>(class: TaskClass, fut: F) -> JoinHandle<F::Output>
where F: Future + Send + 'static, F::Output: Send + 'static;
```

## How it works

A process-singleton `Registry` (in a `OnceLock`) owns one `tokio::Semaphore` per
class plus a `Notify` bulk-gate and a `critical_busy` atomic counter. Permit
counts are computed from `available_parallelism()`:

| Class | Permits |
| --- | --- |
| Critical | `cores * 4` (min 8) |
| Realtime | `cores * 2` (min 4) |
| Sidecar | `cores` (min 2) |
| Background | 2 |
| Bulk | 1 |
| Swarm | effectively unlimited (`1 << 20`); override via `ORIGIN_SWARM_LANE_MAX` |

`spawn_in` acquires an owned permit, then for `Critical` tasks installs a
`CriticalGuard` (RAII) that increments `critical_busy` and decrements it on
**every** exit path — normal return, panic unwind, or cancellation — so the
counter can never leak. For `Bulk` tasks it first awaits `BulkGate::wait_until_idle`,
parking until `critical_busy` reaches zero. When the last `Critical` permit is
released, `note_critical_release` calls `notify_waiters()` to wake parked Bulk
tasks.

```text
spawn_in(class, fut):
   acquire sema[class] permit
   if Critical:  CriticalGuard (busy++ now, busy-- on drop) ─┐
   if Bulk:      BulkGate::wait_until_idle()  ◄── parked until ┘ busy == 0
   poll fut
```

The `Swarm` lane is deliberately a high coarse backstop, not the real limiter —
swarm concurrency is governed by the memory-aware `AdmissionGate` in
`origin-swarm`; the semaphore exists only to keep the "every task takes a permit"
contract intact and cap a runaway. `BulkGate::wait_until_idle` constructs its
`Notified` future before checking the counter to close the lost-wakeup window.

## Dependencies & features

- `tokio` (`rt`, `rt-multi-thread`, `sync`, `macros`, `time`) — runtime,
  semaphores, `Notify`.
- `parking_lot` — the init lock.
- `tracing`, `thiserror`.

No cargo features are defined. Behaviour is tunable at runtime via the
`ORIGIN_SWARM_LANE_MAX` environment variable.

## Used by

Per `Grep "origin-runtime" crates/*/Cargo.toml`: `origin-cas` (uring write hop),
`origin-cli`, `origin-daemon`, `origin-swarm`.

## Testing

`crates/origin-runtime/tests/`: `spawn.rs` (permit acquisition) and
`bulk_gate.rs` (Bulk-parked-by-Critical fairness, using
`critical_tasks_in_flight` for deterministic readiness).

## See also

- [../architecture/runtime-and-concurrency.md](../architecture/runtime-and-concurrency.md) — the task-class model in context.
- [../subsystems/swarm-and-orchestration.md](../subsystems/swarm-and-orchestration.md) — the `Swarm` lane and admission gate.
- Back to [../crates/README.md](../crates/README.md).

_Last reviewed against workspace version 0.9.8._
