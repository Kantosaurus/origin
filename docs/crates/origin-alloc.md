# origin-alloc

> Per-component allocator arenas with a no-op default and an opt-in jemalloc backend

## Purpose

`origin-alloc` lets the daemon attribute heap usage to logical components
(agent loop, CAS, swarm workers, IPC, memory graph, …) by binding allocations to
named arenas. With the default no-op backend it is a zero-cost API; with the
opt-in `jemalloc` backend (unix-only) it routes to jemalloc per-arena MALLCTL
operations so a component's resident bytes can be snapshotted, reset, or fully
destroyed — e.g. reclaiming a swarm worker's memory on exit.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `ArenaId` | enum | Stable per-component arena identifier (10 variants, `COUNT`, `label`, `backend_index`). |
| `ArenaScope` | struct | RAII guard restoring the previous arena binding on drop. |
| `with_arena` | fn | Run a closure with the thread bound to `id`; restores on return. |
| `stats_snapshot` | fn | `[ArenaStat; ArenaId::COUNT]` of resident/allocated bytes (zeros on no-op). |
| `reset` | fn | `arena.<i>.reset` — drop physical pages, keep the arena. |
| `destroy` | fn | `arena.<i>.destroy` — fully invalidate the arena. |
| `AllocError` | enum | `Bind(ArenaId, String)` / `Unavailable`. |
| `ArenaStat` | struct | Per-arena byte snapshot (backend-specific). |
| `JemallocAllocator` | type | Re-export of `tikv_jemallocator::Jemalloc` (jemalloc feature only). |

## Key types

```rust
#[repr(u8)]
pub enum ArenaId {
    Agent = 0, Cas = 1, Sidecar = 2, SwarmCoord = 3, SwarmWorker = 4,
    Ipc = 5, MetricsHttp = 6, CodeGraph = 7, Mem = 8, Other = 9,
}
impl ArenaId { pub const COUNT: usize = 10; }
```

```rust
pub fn with_arena<R>(id: ArenaId, f: impl FnOnce(&ArenaScope) -> R) -> Result<R, AllocError> {
    let prev = backend::bind_thread_arena(id);
    let scope = ArenaScope::new(id, prev);
    let out = f(&scope);
    drop(scope); // Drop restores `prev`.
    Ok(out)
}
```

## How it works

The crate selects a backend at compile time. On `cfg(not(all(feature = "jemalloc", unix)))`
it uses `noop_backend` — `with_arena` runs the closure, snapshots are all zeros,
and `reset`/`destroy` are inert. On `cfg(all(feature = "jemalloc", unix))` it uses
`jemalloc_backend`, which issues the real `mallctl` calls.

`with_arena` binds the *current thread* to the arena, runs the closure, and
restores the previous binding via `ArenaScope`'s `Drop` — so arena scopes nest
correctly. The library deliberately does **not** install a `#[global_allocator]`;
that is the binary's choice (it can opt in with the re-exported
`JemallocAllocator`). Per-arena MALLCTL still works regardless because
`tikv-jemalloc-sys` links jemalloc's symbols in unconditionally.

```text
with_arena(SwarmWorker, |_| { /* allocate */ })   // bound to arena 4
   ... worker exits ...
destroy(ArenaId::SwarmWorker)                      // reclaim its pages
```

The optional jemalloc deps are gated to `cfg(unix)` so `--all-features` still
resolves on Windows, where the `jemalloc` feature becomes a no-op.

## Dependencies & features

- `thiserror`, `tracing`.
- **Feature `jemalloc`** (unix only): `tikv-jemallocator`, `tikv-jemalloc-ctl`,
  `tikv-jemalloc-sys`, `libc`.
- Dev: `serial_test` (arena state is process-global, so tests serialise).
- **`unsafe_code = "allow"`** for the jemalloc FFI; each unsafe block carries a
  `SAFETY:` comment.

## Used by

Per `Grep "origin-alloc" crates/*/Cargo.toml`: `origin-daemon` (and `origin-alloc`
itself). The `ArenaId` variants map onto components across the daemon, CAS,
swarm, IPC, code-graph, and memory subsystems.

## Testing

`crates/origin-alloc/tests/`: `arena_routing.rs` (binding/restore semantics) and
`reset_destroy.rs` (reset/destroy lifecycle). Both use `serial_test`.

## See also

- [../architecture/runtime-and-concurrency.md](../architecture/runtime-and-concurrency.md) — per-component memory attribution.
- [../subsystems/observability.md](../subsystems/observability.md) — arena stats in metrics.
- [../subsystems/swarm-and-orchestration.md](../subsystems/swarm-and-orchestration.md) — `SwarmWorker` arena reclamation.
- Back to [../crates/README.md](../crates/README.md).

_Last reviewed against workspace version 0.9.8._
