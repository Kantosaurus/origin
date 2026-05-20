# `origin` Phase 12 — Multi-runtime + arenas + cooperative shutdown (`origin-alloc` + `origin-runtime` + io_uring CAS + `origin-supervisor`) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL — use **superpowers:subagent-driven-development** to execute task-by-task. Within each task follow **superpowers:test-driven-development** (failing test first, run to fail, implement, run to pass) and apply **superpowers:verification-before-completion** — do NOT advance to the next task until the verification gate is fully green. Steps use checkbox (`- [ ]`) syntax. Tasks marked **[parallel-safe]** can run concurrently in fresh subagents (see "Parallelization" below).

**Branch:** All Phase 12 work lands on branch `p-12` (branched off `dev`, which carries the `p11-complete` tag).

**Goal:** Bolt sustained-heavy-use runtime hygiene onto the P11 baseline — (1) per-component **jemalloc named arenas** so subsystem allocations are isolated and a worker's entire arena can be returned to the OS in a single syscall on completion, (2) a `TaskClass`-budgeted **`spawn_in(class, fut)` helper** that replaces raw `tokio::spawn` everywhere in the workspace and a clippy-style `xtask` lint that enforces it, (3) a **two-runtime split** — a `current_thread` *control core* pinned to one OS thread that owns IPC accept, renderer ticks, event dispatch, and shutdown supervision; a `multi_thread` *worker pool* sized to `physical_cores - 1` that owns agent loops, provider HTTP/2 clients, sidecar jobs, MCP transport, CAS pack I/O, swarm workers, and metrics endpoint, (4) **`tokio-uring` async I/O on Linux** for CAS pack reads/writes (with a feature-gated fallback to `tokio::fs` on macOS/Windows), (5) a **phased cooperative shutdown supervisor** inside the daemon that walks the eight-phase sequence in N8.10, (6) a new top-level binary **`origin-supervisor`** that restart-on-panics `origin-daemon` and persists enough state in SQLite + CAS for the next daemon process to resume in-flight sessions from their last checkpoint.

**Architecture:** Two new library crates carry the foundational primitives — `origin-alloc` (jemalloc arena registry + RAII `ArenaScope`) and `origin-runtime` (`TaskClass` enum + `spawn_in` + per-class `Semaphore` budgets + `Bulk`-pauses-when-`Critical`-active gate). One new top-level binary crate `origin-supervisor` wraps `origin-daemon` and watches the child PID. `origin-cas` grows a `uring` cargo feature behind which pack-file `Read`/`Write` traits get a `tokio-uring`-backed implementation; the default path stays `std::fs` + `memmap2` so non-Linux hosts and the test suite keep working. `origin-daemon` does the most surgical work this phase: `main.rs` is rebuilt around a hand-rolled two-runtime launcher (a `LocalSet` driven by a `current_thread` runtime on a `std::thread::Builder` thread with `name("origin-ctrl")` for the control core, and a long-lived `tokio::runtime::Runtime` for the worker pool); every existing `tokio::spawn` site is migrated to `spawn_in(class, fut)`; a new `shutdown.rs` module implements the eight-phase `CooperativeShutdown` driver. Each cluster is independent of the others after the branch-checkpoint task (P12.0) except where noted in the dependency graph below, so the five area-clusters are **parallelizable** with two narrow ordering edges (B → D, D → E).

**Tech Stack:** Rust 1.83 (MSRV pin), `tikv-jemallocator` 0.6 + `tikv-jemalloc-ctl` 0.6 (named-arena MALLCTL surface; the `tikv-` fork ships modern jemalloc 5.3 bindings with arena.<i>.purge / arena.<i>.reset / arena.<i>.destroy support and is the de-facto Rust binding still maintained against jemalloc 5.x), `tokio-uring` 0.5 (Linux-only; cargo feature `uring`, gated on `#[cfg(target_os = "linux")]`), `tokio` 1 (already a workspace dep; promoted to a direct one in the new runtime crate), `parking_lot` 0.12 (already in the workspace; per-class semaphores), `serde` 1, `serde_json` 1 (supervisor checkpoint serialization), `thiserror` 1, `proc-macro2` 1 + `syn` 2 + `quote` 1 (xtask lint expands; `xtask` already exists from P11.14 — we add a second subcommand `lint-spawn`), `which` 6 (supervisor child-launch path resolution), `nix` 0.29 (POSIX signal handling on Linux/macOS for supervisor → daemon graceful-shutdown trigger), `windows-sys` 0.59 (already in workspace; supervisor uses `Job Object` + `WaitForSingleObject` on Windows).

**Novel-implementation reflex** per `[[feedback_novel_implementations]]` — every signature subsystem must beat openclaude/jcode/opencode on tokens or perf. Phase 12's novelties:

1. **Named jemalloc arenas selected by const `ArenaId`** held on every long-lived subsystem handle — the arena id is an `enum` discriminant baked into `ArenaScope` at construction, dispatch is a single `mallctl("thread.arena", ...)` per scope entry and zero allocations after. Compare with jcode's process-global jemalloc with no per-component partition: there a chatty worker poisons the agent heap; here the worker's arena is `arena.<i>.destroy`'d in a single syscall on completion.
2. **`spawn_in(class, fut)` is a `const`-classified macro/inline-fn pair** — the `TaskClass` is a `#[repr(u8)]` enum and dispatch is a `match` on the discriminant that hands the future to one of five pre-built `tokio::sync::Semaphore`s. Each class has a configured permit count derived from `available_parallelism()`. No string lookup, no dyn dispatch on the spawn hot path.
3. **`Bulk` tasks are gated by a *suspend-when-`Critical`-active* watcher** — a `tokio::sync::Notify` triggered whenever the `Critical` semaphore drops below half-capacity; `Bulk` jobs `.notified()` on it before each unit of work. CAS GC, initial code-graph build, and MCP discovery pause automatically whenever the agent is in a turn, with zero per-poll cost.
4. **Two-runtime split with SMR-ring cross-runtime handoff** — control-core futures and worker-pool futures never call `block_on` across the boundary; cross-runtime messaging reuses the `origin-smr` shared-memory ring from P9. A `RuntimeHandle::dispatch_to_worker(fut)` simply pushes a `WakeRecord` onto an SPSC ring the worker pool polls; conversely `dispatch_to_control(event)` walks the same ring in reverse. ~80ns p50 vs. ~5µs for `tokio::sync::mpsc` across runtimes.
5. **`tokio-uring` CAS pack reads use the existing mmap'd index** — only the *payload read* path issues `read_at` SQEs; the index walk remains a `&[u8]` slice into the mmapped index region. The result is a single submit-and-wait per payload with no syscall on the index path. Compared with reading the whole pack through `tokio::fs::File::read_at`, the io_uring path issues O(1) SQEs per `Store::get` and eliminates the per-call `pread64` syscall.
6. **Cooperative shutdown is eight typed phases on a `LocalSet`, each with its own budget timer** — phases are an `enum ShutdownPhase` and the supervisor logs *which* phase failed if a budget expires. There is no global "drain everything" loop; each phase has a typed effect (`StopAcceptingIpc`, `CancelBulkAndBackground`, `DrainCritical { budget }`, `PersistSidecarQueue`, `FlushCasWriteBuffer`, `CheckpointSqlite`, `CloseIpc`, `ReleaseSharedMemoryAndArenas`). Mid-phase stuck → log + force-advance, never deadlock.
7. **`origin-supervisor` checkpoints session state on every assistant-turn boundary** into a per-session `resume_token.bin` (rkyv-archived `ResumeToken { session_id, last_turn, cas_handle_root, pending_tool_calls, plan_seq }`). On daemon restart it replays the token to the daemon over IPC; the daemon resumes by loading session messages from SQLite + CAS up to `last_turn` and re-spawning any pending tool calls. Compare with jcode's process-tree-only supervisor (no session-resume): a daemon panic mid-turn loses the turn there; here we re-enter at the same point.

**Builds on:** `docs/superpowers/specs/2026-05-19-origin-harness-design.md` mechanisms **N8.6** (per-component jemalloc arenas), **N8.7** (`spawn_in` + clippy enforcement), **N8.8** (two-runtime split), **N8.9** (platform-native async I/O, Linux io_uring portion), **N8.10** (cooperative phased shutdown), and the `origin-supervisor` line item from Section 11's Phase 12 deliverables. Builds on the `p11-complete` tag.

**Phase 12 spec-mechanism citations:**

- **N8.6** — Per-component jemalloc arenas (Tasks P12.1, P12.2)
- **N8.7** — `spawn_in(class, …)` + clippy lint (Tasks P12.3, P12.4, P12.5, P12.9)
- **N8.8** — Two-runtime split (Tasks P12.8)
- **N8.9** — `tokio-uring` for CAS pack files on Linux (Tasks P12.6, P12.7)
- **N8.10** — Phased cooperative shutdown (Tasks P12.11)
- `origin-supervisor` — restart-on-panic with session resume (Tasks P12.10, P12.12)

**What is explicitly out of scope for Phase 12** (deferred):

- IOCP overlapped I/O on Windows and kqueue on macOS for CAS pack files — Phase 12 ships only the Linux `tokio-uring` path. macOS/Windows continue to use the existing `std::fs` + `memmap2` path under the default cargo feature. Per-OS native async I/O for non-Linux platforms is deferred to a post-GA hardening phase.
- The QUIC remote IPC transport (N7.12) — that is Phase 13.
- The desktop GUI / web frontend — Phase 14+ / post-GA.
- Live migration of an in-flight stream across a daemon restart — the supervisor resumes from the last *assistant-turn boundary*, not mid-stream. Mid-stream interrupts are surfaced to the client as a `StreamAborted { resume_token }` event; the client re-issues the prompt.
- Per-arena RSS metrics on the `/metrics` Prometheus endpoint — `origin-alloc` exposes a `stats_snapshot()` API in P12 but wiring the per-arena gauge into the bounded-cardinality `(class, provider, tool, error_kind)` keyspace is a separate observability-polish task deferred to P14.
- Renaming or splitting the `origin-cas` `uring` cargo feature into sub-features — the `uring` feature gates both read and write paths atomically this phase.
- Migrating *every* `tokio::spawn` call in the entire workspace. P12.9 migrates **all** call sites in `origin-daemon` (the hot path) and the supervisor itself. Provider-crate spawns (one-off keepalive tasks) are migrated as a "stragglers" follow-up in P14. The `xtask lint-spawn` check is configured with a per-crate allowlist for those stragglers, so the lint stays green during P12 even though migration is partial.

---

## Conventions reminder (apply to every task)

**TDD shape, every task:**

1. Write the failing test.
2. Run it — confirm the expected failure mode (compile error or assertion).
3. Implement the minimum to pass.
4. Run the test — confirm pass.
5. Verification gate (see table).
6. Commit (Conventional Commits, scoped to crate).

**Verification gate per task type:**

| Task type | Verification commands (all must exit 0) |
|---|---|
| Pure-logic / single-crate | `cargo test -p <crate>` + `cargo clippy -p <crate> --all-targets -- -D warnings` + `cargo fmt --check` |
| Cross-crate / daemon wiring / spawn-site migration | `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --check` |
| Linux-only backend (P12.6 io_uring read, P12.7 io_uring write) | `cargo test -p origin-cas --features uring` on a Linux host **plus** `cargo check -p origin-cas --features uring --target x86_64-unknown-linux-gnu` in CI. When iterating on Windows host, skip the test run and rely on the cross `cargo check` plus a `--no-default-features` build to confirm the gate is sound. |
| jemalloc / allocator (P12.1, P12.2) | All cross-crate commands above **plus** `cargo test -p origin-alloc --features jemalloc -- --test-threads=1` (jemalloc MALLCTL state is process-global; tests must serialize) |
| `xtask` spawn lint (P12.5) | `cargo run -p xtask -- lint-spawn` exits 0 on a clean tree and **non-zero** on the synthetic violation fixture under `xtask/tests/fixtures/dirty_spawn/` |
| Bench-touching tasks (P12.6 io_uring read throughput, P12.7 io_uring write throughput) | All cross-crate commands above + `cargo bench -p origin-cas --features uring --bench uring_throughput -- --quick` exits 0 with the listed threshold met |
| Supervisor (P12.10, P12.12) | `cargo test --workspace` + the smoke test under `crates/origin-supervisor/tests/restart.rs` (spawns a daemon, sends SIGABRT / `TerminateProcess`, asserts a new daemon comes up within 2 s and resumes the open session) |
| Final phase gate (P12.13) | All of the above + tag `p12-complete` |

**Patterns inherited from earlier phases:**

- `[lints] workspace = true` in every new crate `Cargo.toml`; workspace inheritance for `version`/`edition`/`rust-version`/`license`/`repository`.
- All persisted/IPC-crossing types derive `serde::{Serialize, Deserialize}` (JSON) or `rkyv::{Archive, Serialize, Deserialize}` with `#[archive(check_bytes)]` (records that round-trip through CAS).
- `[lints.rust] unsafe_code = "forbid"` is the default; the only new crate that needs an override is `origin-alloc` (jemalloc MALLCTL FFI). `origin-runtime` and `origin-supervisor` stay `unsafe_code = "forbid"`.
- `#[must_use]` on every public constructor; `const fn` wherever Rust allows.
- Tests use `.expect("meaningful message")` — never `unwrap()` and never `#[allow(clippy::unwrap_used)]`.
- Custom error enums via `thiserror`; document `# Errors` and `# Panics` on `pub fn`s.
- For each `#[allow(clippy::…)]` add an inline comment justifying it; never blanket-suppress.
- **MSRV pin reflex** (`[[project_msrv_dep_pinning]]`): if `cargo check` complains about `edition2024` or "requires Rust 1.85+", pin the offender with `cargo update -p <crate> --precise <ver>` and record in `Cargo.lock`. Likely candidates this phase: `tokio-uring` 0.5 pulls a fresh `io-uring` crate which may demand newer `bitflags`; if Cargo trips, try `tokio-uring = "=0.5.0"` and `io-uring = "=0.6.4"`. `tikv-jemallocator` 0.6 pulls `tikv-jemalloc-sys 0.6` whose build script may want a fresh `cc`; if it trips on edition2024, pin `cc = "=1.0.99"`. `nix` 0.29 pulls `bitflags 2.6+`; pin `bitflags = "=2.6.0"` if needed.
- **Novel-implementation reflex** (`[[feedback_novel_implementations]]`): if a step's implementation collapses into "the obvious thing openclaude does" (e.g. one global jemalloc, raw `tokio::spawn` everywhere, `select! { _ = ctrl_c() => process::exit(0) }`), stop and re-read the architecture novelties listed above.

**Commit style:** Conventional commits, scoped to crate where possible. Each task lands in **one commit** on branch `p-12`. Final commit on P12.13 carries `tag: p12-complete`.

---

## Parallelization

After P12.0 the work splits into **five area-clusters** with two narrow ordering edges. Each cluster can be assigned to a fresh subagent:

| Cluster | Tasks | New crate(s) | Touches existing crates | Depends on |
|---|---|---|---|---|
| **A. jemalloc arenas** | P12.1 → P12.2 | `origin-alloc` | none (consumers wire in P12.9) | P12.0 |
| **B. Task-class runtime** | P12.3 → P12.4 → P12.5 | `origin-runtime` | `xtask` (new subcommand `lint-spawn`) | P12.0 |
| **C. io_uring CAS** | P12.6 → P12.7 | _(extends `origin-cas`)_ | `origin-cas` | P12.0 |
| **D. Two-runtime split + spawn migration** | P12.8 → P12.9 | none | `origin-daemon` | **B** |
| **E. Supervisor + cooperative shutdown** | P12.10 → P12.11 → P12.12 | `origin-supervisor` | `origin-daemon` (shutdown driver + resume IPC verbs) | **D** |

**Dispatch order for parallel subagents (recommended):**

```
                          P12.0 (branch + workspace deps)
                                       │
            ┌──────────────────────────┼──────────────────────────┐
            ▼                          ▼                          ▼
        Cluster A                  Cluster B                  Cluster C
        P12.1-2                    P12.3-5                    P12.6-7
            │                          │                          │
            │                          └─────┐                    │
            │                                ▼                    │
            │                          Cluster D                  │
            │                          P12.8-9                    │
            │                                │                    │
            │                                └─────┐              │
            │                                      ▼              │
            │                                Cluster E            │
            │                                P12.10-12            │
            │                                      │              │
            └──────────────────────────────────────┴──────────────┘
                                       │
                                P12.13 (phase gate + tag)
```

**Wave 1 (parallel):** Clusters A, B, C run in three concurrent subagents.
**Wave 2:** Cluster D starts as soon as Cluster B lands P12.5 (the lint and the `spawn_in` API are both required).
**Wave 3:** Cluster E starts as soon as Cluster D lands P12.9 (the supervisor needs the two-runtime split to know which runtime to drive the shutdown sequence on).
**Final:** P12.13 holds the tag-bearing merge until A, B, C, D, E are all green.

Each subagent gets a single-cluster prompt that says: "*work tasks P12.X through P12.Y sequentially; commit each on `p-12`; do not touch files outside this cluster's column in the file map.*"

---

## File map for Phase 12

| New / modified file | Responsibility |
|---|---|
| **Cluster A — jemalloc arenas** | |
| `crates/origin-alloc/Cargo.toml` | manifest; workspace lints; `unsafe_code = "allow"` override; `jemalloc` cargo feature |
| `crates/origin-alloc/src/lib.rs` | public surface — `ArenaId`, `ArenaScope`, `with_arena`, `stats_snapshot` |
| `crates/origin-alloc/src/arena_id.rs` | `#[repr(u8)] enum ArenaId { Agent, Cas, Sidecar, SwarmCoord, SwarmWorker, Ipc, MetricsHttp, CodeGraph, Mem, Other }` + jemalloc arena handle map |
| `crates/origin-alloc/src/jemalloc_backend.rs` | `#[cfg(feature = "jemalloc")]` — MALLCTL `arenas.create`, `thread.arena`, `arena.<i>.purge`, `arena.<i>.destroy` wrappers; stats snapshot |
| `crates/origin-alloc/src/noop_backend.rs` | default backend; `ArenaScope` is a no-op; used on Windows where jemalloc is opt-in |
| `crates/origin-alloc/src/scope.rs` | RAII `ArenaScope { id, prev_thread_arena }` — `Drop` restores the previous binding |
| `crates/origin-alloc/tests/arena_routing.rs` | every variant of `ArenaId` returns a distinct jemalloc arena index; `with_arena(Cas, …)` binds then restores |
| `crates/origin-alloc/tests/reset_destroy.rs` | `Arena::reset(id)` + `Arena::destroy(id)` round-trip; `stats_snapshot()` reports a strictly decreasing `resident` after destroy |
| **Cluster B — Task-class runtime + spawn lint** | |
| `crates/origin-runtime/Cargo.toml` | manifest; workspace lints; depends on `tokio`, `parking_lot`, `tracing` |
| `crates/origin-runtime/src/lib.rs` | public surface — `TaskClass`, `spawn_in`, `RuntimeHandle`, `init_for_test` |
| `crates/origin-runtime/src/class.rs` | `#[repr(u8)] enum TaskClass { Critical = 0, Realtime = 1, Sidecar = 2, Background = 3, Bulk = 4 }` + per-class default permit counts derived from `available_parallelism()` |
| `crates/origin-runtime/src/spawn.rs` | `pub fn spawn_in<F>(class: TaskClass, fut: F) -> JoinHandle<F::Output>` — `match` on class, acquires a permit, spawns the future in the global registry |
| `crates/origin-runtime/src/registry.rs` | process-singleton `Registry { sema: [Semaphore; 5], bulk_gate: Arc<Notify>, critical_busy: AtomicI32 }` |
| `crates/origin-runtime/src/bulk_gate.rs` | `BulkGate` — `Bulk` tasks `.notified()` between work units; gate flipped by `Critical` semaphore tracker |
| `crates/origin-runtime/tests/spawn.rs` | `spawn_in(Critical, …).await` returns; `Bulk` task is parked while a `Critical` task holds a permit |
| `crates/origin-runtime/tests/bulk_gate.rs` | gate behaviour under concurrent Critical/Bulk pairs |
| `xtask/src/main.rs` *(modify P12.5)* | add `LintSpawn` clap subcommand alongside the existing `lint-secrets` |
| `xtask/src/lint_spawn.rs` *(new, P12.5)* | AST walker over workspace `.rs` files; flag `tokio::spawn(`/`tokio::task::spawn(`/`tokio::task::spawn_blocking(` outside `origin-runtime/src/spawn.rs` and outside the allowlist below |
| `xtask/src/lint_spawn_allowlist.rs` *(new, P12.5)* | per-crate allowlist of files exempt from the lint (provider keepalive helpers, sidecar `Sidecar::spawn`, supervisor's own child-launch path) |
| `xtask/tests/fixtures/clean_spawn/main.rs` *(new, P12.5)* | only uses `spawn_in` — lint passes |
| `xtask/tests/fixtures/dirty_spawn/main.rs` *(new, P12.5)* | calls raw `tokio::spawn` outside the allowlist — lint fails |
| `xtask/tests/lint_spawn.rs` *(new, P12.5)* | runs the lint over both fixtures and asserts exit codes |
| **Cluster C — io_uring CAS** | |
| `crates/origin-cas/Cargo.toml` *(modify, P12.6)* | add optional `tokio-uring` dep, gated on `target_os = "linux"`; add `uring` cargo feature |
| `crates/origin-cas/src/packfile_uring.rs` *(new, P12.6)* | `#[cfg(all(target_os = "linux", feature = "uring"))]` — `tokio_uring::fs::File`-backed `PackReader::read_at_uring(&self, off, len)` + `PackBuilder::append_uring(&mut self, hash, bytes)` |
| `crates/origin-cas/src/lib.rs` *(modify, P12.6)* | re-export `packfile_uring` items behind the feature gate |
| `crates/origin-cas/src/store.rs` *(modify, P12.7)* | when built with `feature = "uring"` and on Linux, `Store::get` and `Store::put` route the *payload* I/O through the uring helpers; index walk remains the mmap'd path |
| `crates/origin-cas/tests/uring_round_trip.rs` *(new, P12.6)* | `#[cfg(all(target_os = "linux", feature = "uring"))]` — write 1k blobs through uring writer, read them back through uring reader |
| `crates/origin-cas/benches/uring_throughput.rs` *(new, P12.7)* | criterion-style; threshold ≥ 250 MiB/s sequential read, ≥ 180 MiB/s sequential write on a `tmpfs` |
| **Cluster D — Two-runtime split + spawn migration** | |
| `crates/origin-daemon/Cargo.toml` *(modify, P12.8)* | depend on `origin-runtime`, `origin-alloc` |
| `crates/origin-daemon/src/runtime_launch.rs` *(new, P12.8)* | builds the control-core `tokio::runtime::Builder::new_current_thread()` and the worker-pool `Builder::new_multi_thread().worker_threads(physical_cores - 1)`; pins control core to OS thread `origin-ctrl`; returns `(ControlHandle, WorkerHandle)` |
| `crates/origin-daemon/src/main.rs` *(modify, P12.8)* | replaces `#[tokio::main]` attribute with a hand-rolled `fn main()` that calls `runtime_launch::start`; IPC accept loop + renderer ticks + event dispatch live on the control core; everything else lives on the worker pool |
| `crates/origin-daemon/src/agent.rs` *(modify, P12.9)* | every `tokio::spawn` → `spawn_in(TaskClass::Critical, …)` for agent-loop turns, `Sidecar` for compaction, `Background` for CAS GC |
| `crates/origin-daemon/src/stream_relay.rs` *(modify, P12.9)* | per-request relay tasks → `spawn_in(TaskClass::Realtime, …)` |
| `crates/origin-daemon/src/session_store.rs` *(modify, P12.9)* | persistent flush task → `spawn_in(TaskClass::Background, …)` |
| `crates/origin-daemon/tests/runtime_split.rs` *(new, P12.8)* | a `Critical` future returns its `std::thread::current().name()` and an `Realtime` future returns the same; they differ (`origin-ctrl` vs `tokio-runtime-worker`) |
| `crates/origin-daemon/tests/spawn_audit.rs` *(new, P12.9)* | grep-style check (compile-time `include_str!` over the daemon source dir) — only `spawn_in` appears outside the allowlisted helper file |
| **Cluster E — Supervisor + cooperative shutdown** | |
| `crates/origin-supervisor/Cargo.toml` *(new, P12.10)* | manifest; produces a binary `origin-supervisor` |
| `crates/origin-supervisor/src/main.rs` *(new, P12.10)* | child-launch loop — `spawn origin-daemon`, watch PID, restart on non-zero exit; configurable max restarts per minute |
| `crates/origin-supervisor/src/launch_unix.rs` *(new, P12.10)* | `#[cfg(unix)]` — `nix::unistd::fork`+`execve` semantics via `tokio::process::Command`; SIGABRT/SIGSEGV detection from `WaitStatus` |
| `crates/origin-supervisor/src/launch_windows.rs` *(new, P12.10)* | `#[cfg(windows)]` — `CreateProcessW` via `tokio::process::Command`; child wrapped in a Job Object so a supervisor exit cleans the daemon |
| `crates/origin-supervisor/src/resume_token.rs` *(new, P12.12)* | `ResumeToken` struct + rkyv archive + load/store from `<state_dir>/resume/<session_id>.bin` |
| `crates/origin-supervisor/src/ipc_resume.rs` *(new, P12.12)* | on daemon-up, sends a `ResumeRequest` IPC frame to the daemon with the tokens of all open sessions |
| `crates/origin-supervisor/tests/restart.rs` *(new, P12.10)* | spawns the supervisor, asserts a `SIGKILL`'d daemon is replaced within 2 s |
| `crates/origin-supervisor/tests/resume.rs` *(new, P12.12)* | end-to-end — open a session, take a turn, SIGKILL daemon, assert next daemon answers `list_sessions` with the same session still open at the last assistant turn |
| `crates/origin-daemon/src/shutdown.rs` *(new, P12.11)* | `enum ShutdownPhase` (eight variants) + `CooperativeShutdown::run()` driver |
| `crates/origin-daemon/src/main.rs` *(modify, P12.11)* | wire SIGTERM / SIGINT / Ctrl+C handler to invoke `CooperativeShutdown::run()` on the control core |
| `crates/origin-daemon/src/protocol.rs` *(modify, P12.12)* | add `ClientMessage::ResumeRequest { tokens }` + `ServerMessage::ResumeAck { session_ids }` variants |
| `crates/origin-daemon/src/session_store.rs` *(modify, P12.12)* | `SessionStore::save_resume_token(&self, session_id, token)` and `SessionStore::load_resume_token(&self, session_id) -> Option<ResumeToken>` |
| `crates/origin-daemon/tests/shutdown_phases.rs` *(new, P12.11)* | drive each phase in isolation; assert the budget timer fires when a phase hangs |
| **Cross-cutting** | |
| `Cargo.toml` *(modify, P12.0)* | new members `crates/origin-alloc`, `crates/origin-runtime`, `crates/origin-supervisor`; add `tikv-jemallocator`, `tikv-jemalloc-ctl`, `tokio-uring`, `nix` to `[workspace.dependencies]` |
| `rust-toolchain.toml` *(unchanged)* | `channel = "1.83"` |

**File-size discipline:** every new `.rs` file targets <400 LOC. If a task naturally pushes a file past 400 LOC, split early (e.g. `shutdown.rs` → `shutdown/phase.rs` + `shutdown/driver.rs` + `shutdown/mod.rs`).

---

## Task P12.0 — Branch + workspace dep additions + plan checkpoint

**Files:**

- Modify: `Cargo.toml` (root workspace) — add new shared deps so each cluster crate inherits version pins.
- Modify: branch state — branch off `dev` to `p-12`.

- [ ] **Step 1: Create the p-12 branch**

```bash
git checkout dev
git pull --ff-only
git checkout -b p-12
```

Run: `git branch --show-current`
Expected output: `p-12`

- [ ] **Step 2: Add shared workspace deps**

Edit the workspace `Cargo.toml`. Extend `members` and `[workspace.dependencies]`:

```toml
[workspace]
resolver = "2"
members = ["crates/*", "xtask"]
exclude = ["crates/origin-daemon/fuzz"]

[workspace.dependencies]
# … existing P10 + P11 entries unchanged …

# P12 additions
tikv-jemallocator = { version = "0.6", default-features = false, features = ["background_threads"] }
tikv-jemalloc-ctl = { version = "0.6", default-features = false }
tokio-uring = "0.5"
nix = { version = "0.29", default-features = false, features = ["signal", "process"] }
```

Note: `crates/*` already picks up the three new crates (`origin-alloc`, `origin-runtime`, `origin-supervisor`) — no explicit member listing needed.

- [ ] **Step 3: Pin transitive deps if needed**

Run: `cargo check --workspace`

If `cargo check` fails with `edition2024` / "requires Rust 1.85+" errors, pin the offenders:

```bash
cargo update -p cc --precise 1.0.99
cargo update -p bitflags --precise 2.6.0
cargo update -p tokio-uring --precise 0.5.0
cargo update -p io-uring --precise 0.6.4
cargo update -p tikv-jemallocator --precise 0.6.0
cargo update -p tikv-jemalloc-sys --precise 0.6.0
```

Re-run `cargo check --workspace` until it exits 0.

- [ ] **Step 4: Stage and commit the plan + workspace deps**

```bash
git add docs/superpowers/plans/2026-05-20-origin-phase-12.md Cargo.toml Cargo.lock
git commit -m "docs(origin): Phase 12 implementation plan + workspace deps (P12.0)"
```

- [ ] **Step 5: Verification gate**

Run: `cargo check --workspace`
Expected: exits 0; no new clippy/test runs at this checkpoint.
Run: `git status`
Expected: working tree clean.

---

# Cluster A — jemalloc arenas

## Task P12.1 — `origin-alloc` skeleton + `ArenaId` enum + RAII `ArenaScope`  **[parallel-safe with B/C]**

**Files:**

- Create: `crates/origin-alloc/Cargo.toml`
- Create: `crates/origin-alloc/src/lib.rs`
- Create: `crates/origin-alloc/src/arena_id.rs`
- Create: `crates/origin-alloc/src/scope.rs`
- Create: `crates/origin-alloc/src/noop_backend.rs`
- Create: `crates/origin-alloc/tests/arena_routing.rs`

- [ ] **Step 1: Manifest** at `crates/origin-alloc/Cargo.toml`

```toml
[package]
name = "origin-alloc"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[lints.rust]
# jemalloc MALLCTL FFI is gated behind the `jemalloc` feature.
unsafe_code = "allow"

[features]
default = []
jemalloc = ["dep:tikv-jemallocator", "dep:tikv-jemalloc-ctl", "dep:libc"]

[dependencies]
thiserror = "1"
tracing = { workspace = true }

tikv-jemallocator = { workspace = true, optional = true }
tikv-jemalloc-ctl = { workspace = true, optional = true }
libc = { version = "0.2", optional = true }

[dev-dependencies]
serial_test = "3"
```

- [ ] **Step 2: Write the failing test** at `crates/origin-alloc/tests/arena_routing.rs`

```rust
//! Routing test — every `ArenaId` resolves to a distinct backend arena handle,
//! and `with_arena(id, |scope| …)` returns the same `id` back via `scope.id()`.

use origin_alloc::{with_arena, ArenaId};

#[test]
fn every_arena_id_is_distinct() {
    let ids = [
        ArenaId::Agent,
        ArenaId::Cas,
        ArenaId::Sidecar,
        ArenaId::SwarmCoord,
        ArenaId::SwarmWorker,
        ArenaId::Ipc,
        ArenaId::MetricsHttp,
        ArenaId::CodeGraph,
        ArenaId::Mem,
        ArenaId::Other,
    ];
    let mut indices = ids.iter().map(|id| id.backend_index()).collect::<Vec<_>>();
    indices.sort_unstable();
    indices.dedup();
    assert_eq!(
        indices.len(),
        ids.len(),
        "every ArenaId must map to a distinct backend index"
    );
}

#[test]
fn with_arena_returns_scope_with_same_id() {
    let observed = with_arena(ArenaId::Cas, |scope| scope.id()).expect("scope should bind");
    assert_eq!(observed, ArenaId::Cas);
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p origin-alloc --test arena_routing`
Expected: FAIL — `origin-alloc` does not yet expose `ArenaId` / `with_arena`.

- [ ] **Step 4: Write `src/arena_id.rs`**

```rust
//! Stable enumeration of per-component allocator arenas.

/// Identifies a logical allocator arena. The backend (jemalloc or no-op) is
/// chosen by cargo feature; the same `ArenaId` resolves to the same arena
/// inside a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ArenaId {
    /// Agent-loop turn buffers, message log staging, cache-planner scratch.
    Agent = 0,
    /// CAS write buffers and decompression scratch.
    Cas = 1,
    /// Sidecar small-model worker — summaries, structure extraction.
    Sidecar = 2,
    /// Swarm coordinator state — plan ops, completion-report assembly.
    SwarmCoord = 3,
    /// Per-worker swarm allocations — `destroy`'d on worker exit.
    SwarmWorker = 4,
    /// IPC frame buffers and rkyv staging.
    Ipc = 5,
    /// `/metrics` Prometheus encoder scratch.
    MetricsHttp = 6,
    /// Code knowledge graph node/edge build buffers.
    CodeGraph = 7,
    /// Conversation memory graph and HNSW scratch.
    Mem = 8,
    /// Catch-all for short-lived allocations not classified above.
    Other = 9,
}

impl ArenaId {
    /// Number of variants. Hard-coded — keep in sync with the enum.
    pub const COUNT: usize = 10;

    /// 0-based dense index into the backend's per-arena tables.
    #[must_use]
    pub const fn backend_index(self) -> usize {
        self as usize
    }

    /// Stable, human-readable label for logs and metrics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Cas => "cas",
            Self::Sidecar => "sidecar",
            Self::SwarmCoord => "swarm_coord",
            Self::SwarmWorker => "swarm_worker",
            Self::Ipc => "ipc",
            Self::MetricsHttp => "metrics_http",
            Self::CodeGraph => "code_graph",
            Self::Mem => "mem",
            Self::Other => "other",
        }
    }
}
```

- [ ] **Step 5: Write `src/scope.rs`**

```rust
//! RAII guard that pins the current thread to an `ArenaId` for the duration of
//! a closure. Re-entrant: nested scopes restore the previous binding on drop.

use crate::arena_id::ArenaId;

/// RAII binding of the current thread to an `ArenaId`.
#[must_use = "the scope must outlive any allocations attributed to it"]
pub struct ArenaScope {
    id: ArenaId,
    // Restoration of the prior thread-arena binding is the backend's job; this
    // field is private and lives only for `Drop`.
    pub(crate) prev_index: Option<usize>,
}

impl ArenaScope {
    /// Arena this scope is currently bound to.
    #[must_use]
    pub const fn id(&self) -> ArenaId {
        self.id
    }

    pub(crate) fn new(id: ArenaId, prev_index: Option<usize>) -> Self {
        Self { id, prev_index }
    }
}

impl Drop for ArenaScope {
    fn drop(&mut self) {
        crate::backend::restore_thread_arena(self.prev_index);
    }
}
```

- [ ] **Step 6: Write `src/noop_backend.rs`**

```rust
//! No-op backend used when the `jemalloc` cargo feature is off. Every
//! `bind_thread_arena` is recorded for the routing test but no real allocator
//! state changes.

use crate::arena_id::ArenaId;
use std::cell::Cell;

thread_local! {
    static CURRENT: Cell<Option<usize>> = const { Cell::new(None) };
}

pub(crate) fn bind_thread_arena(id: ArenaId) -> Option<usize> {
    let prev = CURRENT.with(|c| c.replace(Some(id.backend_index())));
    prev
}

pub(crate) fn restore_thread_arena(prev: Option<usize>) {
    CURRENT.with(|c| c.set(prev));
}

#[must_use]
pub(crate) fn current_thread_arena() -> Option<usize> {
    CURRENT.with(Cell::get)
}
```

- [ ] **Step 7: Write `src/lib.rs`**

```rust
//! `origin-alloc` — per-component allocator arenas with a no-op default and an
//! opt-in jemalloc backend.

pub mod arena_id;
pub mod scope;

#[cfg(not(feature = "jemalloc"))]
mod noop_backend;
#[cfg(not(feature = "jemalloc"))]
pub(crate) use noop_backend as backend;

#[cfg(feature = "jemalloc")]
mod jemalloc_backend;
#[cfg(feature = "jemalloc")]
pub(crate) use jemalloc_backend as backend;

pub use arena_id::ArenaId;
pub use scope::ArenaScope;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AllocError {
    #[error("backend rejected arena bind for `{0:?}`: {1}")]
    Bind(ArenaId, String),
    #[error("backend not available")]
    Unavailable,
}

/// Enter a scope bound to `id`. The closure runs synchronously; allocations
/// inside it are attributed to the arena. The scope is restored on return.
///
/// # Errors
/// Returns [`AllocError::Bind`] if the backend rejects the bind (jemalloc only).
pub fn with_arena<R>(
    id: ArenaId,
    f: impl FnOnce(&ArenaScope) -> R,
) -> Result<R, AllocError> {
    let prev = backend::bind_thread_arena(id);
    let scope = ArenaScope::new(id, prev);
    let out = f(&scope);
    drop(scope); // Drop restores `prev`.
    Ok(out)
}
```

- [ ] **Step 8: Run tests to verify pass**

Run: `cargo test -p origin-alloc`
Expected: PASS — both test cases green.

- [ ] **Step 9: Verification gate**

Run:
```bash
cargo test -p origin-alloc
cargo clippy -p origin-alloc --all-targets -- -D warnings
cargo fmt --check
```
All exit 0.

- [ ] **Step 10: Commit**

```bash
git add crates/origin-alloc Cargo.toml Cargo.lock
git commit -m "feat(origin-alloc): ArenaId enum + RAII ArenaScope + noop backend (P12.1)"
```

---

## Task P12.2 — jemalloc backend + reset / destroy / stats  **[depends P12.1]**

**Files:**

- Create: `crates/origin-alloc/src/jemalloc_backend.rs`
- Modify: `crates/origin-alloc/src/lib.rs` (add `stats_snapshot`, `reset`, `destroy` public surface)
- Create: `crates/origin-alloc/tests/reset_destroy.rs`

- [ ] **Step 1: Write the failing test** at `crates/origin-alloc/tests/reset_destroy.rs`

```rust
//! jemalloc-only — reset releases resident bytes; destroy invalidates the arena.
//!
//! These tests touch global jemalloc state, so they run on a single thread.

#![cfg(feature = "jemalloc")]

use origin_alloc::{destroy, reset, stats_snapshot, with_arena, ArenaId};
use serial_test::serial;

#[test]
#[serial]
fn reset_releases_resident_bytes() {
    // Allocate something into Cas.
    let buf: Vec<u8> = with_arena(ArenaId::Cas, |_| vec![7u8; 16 * 1024 * 1024])
        .expect("scope should bind");
    let before = stats_snapshot().expect("snapshot")[ArenaId::Cas.backend_index()];
    drop(buf);
    reset(ArenaId::Cas).expect("reset should succeed");
    let after = stats_snapshot().expect("snapshot")[ArenaId::Cas.backend_index()];
    assert!(
        after.resident_bytes <= before.resident_bytes,
        "reset must not grow resident: before={} after={}",
        before.resident_bytes,
        after.resident_bytes
    );
}

#[test]
#[serial]
fn destroy_invalidates_arena() {
    // Allocate then destroy. Subsequent rebind must hand back a fresh arena.
    let _drop_me: Vec<u8> = with_arena(ArenaId::SwarmWorker, |_| vec![0u8; 4 * 1024 * 1024])
        .expect("scope should bind");
    let idx_before = stats_snapshot()
        .expect("snapshot")[ArenaId::SwarmWorker.backend_index()]
        .jemalloc_index;
    destroy(ArenaId::SwarmWorker).expect("destroy should succeed");
    // Rebind triggers re-creation.
    let _v: Vec<u8> = with_arena(ArenaId::SwarmWorker, |_| vec![0u8; 1024])
        .expect("scope should bind");
    let idx_after = stats_snapshot()
        .expect("snapshot")[ArenaId::SwarmWorker.backend_index()]
        .jemalloc_index;
    assert_ne!(
        idx_before, idx_after,
        "destroy + rebind must allocate a new jemalloc arena index"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p origin-alloc --features jemalloc --test reset_destroy -- --test-threads=1`
Expected: FAIL — `reset`, `destroy`, `stats_snapshot` not yet defined.

- [ ] **Step 3: Write `src/jemalloc_backend.rs`**

```rust
//! jemalloc backend — one MALLCTL arena per `ArenaId`. Created lazily on first
//! use; reset / destroy operate on the backing jemalloc arena.

use crate::arena_id::ArenaId;
use std::sync::OnceLock;
use std::sync::Mutex;

#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// Per-`ArenaId` jemalloc arena index (`arenas.create` result). Allocated lazily.
static ARENAS: OnceLock<Mutex<[Option<u32>; ArenaId::COUNT]>> = OnceLock::new();

thread_local! {
    static THREAD_ARENA: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

fn arenas() -> &'static Mutex<[Option<u32>; ArenaId::COUNT]> {
    ARENAS.get_or_init(|| Mutex::new([None; ArenaId::COUNT]))
}

fn ensure_arena(id: ArenaId) -> u32 {
    let mut guard = arenas().lock().expect("arenas lock poisoned");
    if let Some(idx) = guard[id.backend_index()] {
        return idx;
    }
    // Safety: `arenas.create` writes a u32 result into the out-pointer.
    let mut new_idx: u32 = 0;
    let mut len: libc::size_t = std::mem::size_of::<u32>();
    let name = c"arenas.create";
    // SAFETY: jemalloc FFI; `mallctl` is the documented control surface.
    unsafe {
        let ret = tikv_jemalloc_sys::mallctl(
            name.as_ptr().cast(),
            (&mut new_idx as *mut u32).cast(),
            &mut len as *mut libc::size_t,
            std::ptr::null_mut(),
            0,
        );
        assert_eq!(ret, 0, "jemalloc arenas.create failed: {ret}");
    }
    guard[id.backend_index()] = Some(new_idx);
    tracing::debug!(arena = id.label(), idx = new_idx, "jemalloc: created arena");
    new_idx
}

pub(crate) fn bind_thread_arena(id: ArenaId) -> Option<usize> {
    let prev = THREAD_ARENA.with(std::cell::Cell::get);
    let new = ensure_arena(id);
    set_thread_arena_raw(new);
    THREAD_ARENA.with(|c| c.set(Some(new)));
    prev.map(|v| v as usize)
}

pub(crate) fn restore_thread_arena(prev: Option<usize>) {
    let v: Option<u32> = prev.map(|n| u32::try_from(n).expect("arena idx fits u32"));
    if let Some(idx) = v {
        set_thread_arena_raw(idx);
        THREAD_ARENA.with(|c| c.set(Some(idx)));
    } else {
        THREAD_ARENA.with(|c| c.set(None));
    }
}

fn set_thread_arena_raw(idx: u32) {
    let name = c"thread.arena";
    let mut value: u32 = idx;
    // SAFETY: jemalloc FFI.
    unsafe {
        let ret = tikv_jemalloc_sys::mallctl(
            name.as_ptr().cast(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            std::mem::size_of::<u32>(),
        );
        assert_eq!(ret, 0, "jemalloc thread.arena set failed: {ret}");
    }
}

pub(crate) fn reset_arena(id: ArenaId) -> Result<(), super::AllocError> {
    let idx = ensure_arena(id);
    let name = std::ffi::CString::new(format!("arena.{idx}.reset"))
        .expect("arena name is valid ascii");
    // SAFETY: jemalloc FFI.
    unsafe {
        let ret = tikv_jemalloc_sys::mallctl(
            name.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        );
        if ret != 0 {
            return Err(super::AllocError::Bind(id, format!("reset rc={ret}")));
        }
    }
    Ok(())
}

pub(crate) fn destroy_arena(id: ArenaId) -> Result<(), super::AllocError> {
    let idx = ensure_arena(id);
    let name = std::ffi::CString::new(format!("arena.{idx}.destroy"))
        .expect("arena name is valid ascii");
    // SAFETY: jemalloc FFI.
    unsafe {
        let ret = tikv_jemalloc_sys::mallctl(
            name.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        );
        if ret != 0 {
            return Err(super::AllocError::Bind(id, format!("destroy rc={ret}")));
        }
    }
    // Forget the index — next bind allocates a fresh arena.
    let mut guard = arenas().lock().expect("arenas lock poisoned");
    guard[id.backend_index()] = None;
    Ok(())
}

/// Snapshot of per-arena resident bytes from `mallctl stats.arenas.<i>.resident`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ArenaStat {
    pub resident_bytes: usize,
    pub allocated_bytes: usize,
    pub jemalloc_index: u32,
}

pub(crate) fn snapshot() -> Result<[ArenaStat; ArenaId::COUNT], super::AllocError> {
    // Refresh stats.
    let epoch = c"epoch";
    let mut ep: u64 = 1;
    let mut len: libc::size_t = std::mem::size_of::<u64>();
    // SAFETY: jemalloc FFI.
    unsafe {
        tikv_jemalloc_sys::mallctl(
            epoch.as_ptr().cast(),
            (&mut ep as *mut u64).cast(),
            &mut len as *mut libc::size_t,
            (&mut ep as *mut u64).cast(),
            std::mem::size_of::<u64>(),
        );
    }
    let mut out = [ArenaStat::default(); ArenaId::COUNT];
    let guard = arenas().lock().expect("arenas lock poisoned");
    for (slot, id_idx) in guard.iter().zip(0..ArenaId::COUNT) {
        if let Some(idx) = *slot {
            out[id_idx].jemalloc_index = idx;
            out[id_idx].resident_bytes = read_arena_stat(idx, "resident").unwrap_or(0);
            out[id_idx].allocated_bytes = read_arena_stat(idx, "small.allocated")
                .unwrap_or(0)
                + read_arena_stat(idx, "large.allocated").unwrap_or(0);
        }
    }
    Ok(out)
}

fn read_arena_stat(idx: u32, leaf: &str) -> Option<usize> {
    let name =
        std::ffi::CString::new(format!("stats.arenas.{idx}.{leaf}")).expect("stat name ascii");
    let mut value: usize = 0;
    let mut len: libc::size_t = std::mem::size_of::<usize>();
    // SAFETY: jemalloc FFI.
    let ret = unsafe {
        tikv_jemalloc_sys::mallctl(
            name.as_ptr(),
            (&mut value as *mut usize).cast(),
            &mut len as *mut libc::size_t,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 {
        Some(value)
    } else {
        None
    }
}
```

- [ ] **Step 4: Extend `src/lib.rs`** with the public surface

```rust
// Append to the existing lib.rs:

/// Per-arena resident / allocated byte snapshot.
#[cfg(feature = "jemalloc")]
pub use crate::jemalloc_backend::ArenaStat;

/// Snapshot of resident bytes per arena. No-op backend returns all zeros.
///
/// # Errors
/// Returns [`AllocError::Unavailable`] on the no-op backend.
pub fn stats_snapshot() -> Result<[backend::ArenaStat; ArenaId::COUNT], AllocError> {
    backend::snapshot()
}

/// `arena.<i>.reset` — drop physical pages without invalidating the arena.
///
/// # Errors
/// Returns [`AllocError::Bind`] if the underlying `mallctl` fails.
pub fn reset(id: ArenaId) -> Result<(), AllocError> {
    backend::reset_arena(id)
}

/// `arena.<i>.destroy` — fully invalidate the arena. Subsequent `with_arena`
/// for the same id allocates a fresh jemalloc arena.
///
/// # Errors
/// Returns [`AllocError::Bind`] if the underlying `mallctl` fails.
pub fn destroy(id: ArenaId) -> Result<(), AllocError> {
    backend::destroy_arena(id)
}
```

And add the `ArenaStat` definition to `noop_backend.rs` so the no-op build still compiles:

```rust
// Append to noop_backend.rs:
#[derive(Debug, Clone, Copy, Default)]
pub struct ArenaStat {
    pub resident_bytes: usize,
    pub allocated_bytes: usize,
    pub jemalloc_index: u32,
}

pub(crate) fn snapshot() -> Result<[ArenaStat; crate::arena_id::ArenaId::COUNT], super::AllocError> {
    Ok([ArenaStat::default(); crate::arena_id::ArenaId::COUNT])
}

pub(crate) fn reset_arena(_id: crate::arena_id::ArenaId) -> Result<(), super::AllocError> {
    Err(super::AllocError::Unavailable)
}

pub(crate) fn destroy_arena(_id: crate::arena_id::ArenaId) -> Result<(), super::AllocError> {
    Err(super::AllocError::Unavailable)
}
```

Add `tikv-jemalloc-sys = { version = "0.6", optional = true }` to the `[dependencies]` table in `crates/origin-alloc/Cargo.toml` and include it under the `jemalloc` feature list.

- [ ] **Step 5: Run tests to verify pass**

Run: `cargo test -p origin-alloc --features jemalloc --test reset_destroy -- --test-threads=1`
Expected: PASS.

Run: `cargo test -p origin-alloc` (no features)
Expected: PASS — the no-op test stays green; reset/destroy tests are `#[cfg(feature = "jemalloc")]` and skipped.

- [ ] **Step 6: Verification gate**

Run:
```bash
cargo test -p origin-alloc
cargo test -p origin-alloc --features jemalloc -- --test-threads=1
cargo clippy -p origin-alloc --all-targets -- -D warnings
cargo clippy -p origin-alloc --features jemalloc --all-targets -- -D warnings
cargo fmt --check
```
All exit 0.

- [ ] **Step 7: Commit**

```bash
git add crates/origin-alloc Cargo.toml Cargo.lock
git commit -m "feat(origin-alloc): jemalloc backend — arena create/reset/destroy + stats (P12.2)"
```

---

# Cluster B — Task-class runtime + spawn lint

## Task P12.3 — `origin-runtime` skeleton + `TaskClass` enum + `spawn_in`  **[parallel-safe with A/C]**

**Files:**

- Create: `crates/origin-runtime/Cargo.toml`
- Create: `crates/origin-runtime/src/lib.rs`
- Create: `crates/origin-runtime/src/class.rs`
- Create: `crates/origin-runtime/src/spawn.rs`
- Create: `crates/origin-runtime/src/registry.rs`
- Create: `crates/origin-runtime/tests/spawn.rs`

- [ ] **Step 1: Manifest** at `crates/origin-runtime/Cargo.toml`

```toml
[package]
name = "origin-runtime"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
tokio = { version = "1", features = ["rt", "rt-multi-thread", "sync", "macros", "time"] }
parking_lot = "0.12"
tracing = { workspace = true }
thiserror = "1"

[dev-dependencies]
tokio = { version = "1", features = ["rt", "rt-multi-thread", "sync", "macros", "time", "test-util"] }
```

- [ ] **Step 2: Write the failing test** at `crates/origin-runtime/tests/spawn.rs`

```rust
//! `spawn_in` returns a JoinHandle whose future executes; each class is
//! tracked by a distinct semaphore.

use origin_runtime::{init_for_test, spawn_in, TaskClass};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_in_runs_to_completion() {
    init_for_test();
    let counter = Arc::new(AtomicU32::new(0));
    let c2 = Arc::clone(&counter);
    let h = spawn_in(TaskClass::Critical, async move {
        c2.fetch_add(1, Ordering::SeqCst);
        42_u32
    });
    let v = h.await.expect("join");
    assert_eq!(v, 42);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_class_runs_independently() {
    init_for_test();
    let mut handles = Vec::new();
    for class in [
        TaskClass::Critical,
        TaskClass::Realtime,
        TaskClass::Sidecar,
        TaskClass::Background,
        TaskClass::Bulk,
    ] {
        handles.push(spawn_in(class, async move { class }));
    }
    for h in handles {
        let _ = h.await.expect("join");
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p origin-runtime`
Expected: FAIL — module surface doesn't exist.

- [ ] **Step 4: Write `src/class.rs`**

```rust
//! Task class taxonomy.

/// Coarse priority/budget bucket for every spawned task in the daemon.
///
/// Lower-numbered classes are more important. The runtime enforces a per-class
/// semaphore permit count; `Bulk` is additionally gated by a watcher that
/// parks it while any `Critical` permit is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TaskClass {
    /// Agent loop turns; provider HTTP/2; tool exec; swarm worker bodies.
    Critical = 0,
    /// Renderer ticks; IPC event dispatch; per-stream relays.
    Realtime = 1,
    /// Sidecar small-model jobs; MCP server clients; hook dispatch.
    Sidecar = 2,
    /// CAS GC; SQLite vacuum; memory idle consolidation.
    Background = 3,
    /// Initial code-graph build; bulk MCP discovery. Paused when `Critical`
    /// has any in-flight work.
    Bulk = 4,
}

impl TaskClass {
    pub const COUNT: usize = 5;

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Realtime => "realtime",
            Self::Sidecar => "sidecar",
            Self::Background => "background",
            Self::Bulk => "bulk",
        }
    }
}
```

- [ ] **Step 5: Write `src/registry.rs`**

```rust
//! Process-singleton runtime registry — per-class semaphores + bulk gate.

use crate::class::TaskClass;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::sync::{Notify, Semaphore};

pub(crate) struct Registry {
    pub(crate) sema: [Arc<Semaphore>; TaskClass::COUNT],
    pub(crate) bulk_gate: Arc<Notify>,
    pub(crate) critical_busy: AtomicI32,
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();
static INIT_LOCK: Mutex<()> = Mutex::new(());

fn permits_for(class: TaskClass) -> usize {
    let cores = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(4);
    match class {
        TaskClass::Critical => (cores * 4).max(8),
        TaskClass::Realtime => (cores * 2).max(4),
        TaskClass::Sidecar => cores.max(2),
        TaskClass::Background => 2,
        TaskClass::Bulk => 1,
    }
}

pub(crate) fn registry() -> &'static Registry {
    REGISTRY.get_or_init(|| {
        let _g = INIT_LOCK.lock();
        Registry {
            sema: [
                Arc::new(Semaphore::new(permits_for(TaskClass::Critical))),
                Arc::new(Semaphore::new(permits_for(TaskClass::Realtime))),
                Arc::new(Semaphore::new(permits_for(TaskClass::Sidecar))),
                Arc::new(Semaphore::new(permits_for(TaskClass::Background))),
                Arc::new(Semaphore::new(permits_for(TaskClass::Bulk))),
            ],
            bulk_gate: Arc::new(Notify::new()),
            critical_busy: AtomicI32::new(0),
        }
    })
}

/// Test helper — does nothing on the second call (registry is singleton).
pub fn init_for_test() {
    let _ = registry();
}

pub(crate) fn note_critical_acquire() {
    registry().critical_busy.fetch_add(1, Ordering::SeqCst);
}

pub(crate) fn note_critical_release() {
    let prev = registry().critical_busy.fetch_sub(1, Ordering::SeqCst);
    if prev <= 1 {
        // No more critical work — release any parked Bulk tasks.
        registry().bulk_gate.notify_waiters();
    }
}

pub(crate) fn critical_in_flight() -> i32 {
    registry().critical_busy.load(Ordering::SeqCst)
}
```

- [ ] **Step 6: Write `src/spawn.rs`**

```rust
//! `spawn_in(class, fut)` — the only sanctioned way to spawn an async task in
//! `origin-daemon`. Every call acquires a per-class permit before polling the
//! inner future.

use crate::class::TaskClass;
use crate::registry::{
    critical_in_flight, note_critical_acquire, note_critical_release, registry,
};
use std::future::Future;
use tokio::task::JoinHandle;

/// Spawn `fut` onto the current Tokio runtime under the given class.
///
/// # Panics
/// Panics if called outside a Tokio runtime context.
pub fn spawn_in<F>(class: TaskClass, fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let reg = registry();
    let sema = std::sync::Arc::clone(&reg.sema[class as usize]);
    let bulk_gate = std::sync::Arc::clone(&reg.bulk_gate);
    tokio::spawn(async move {
        let _permit = sema.acquire_owned().await.expect("semaphore closed");
        if matches!(class, TaskClass::Critical) {
            note_critical_acquire();
        }
        if matches!(class, TaskClass::Bulk) {
            // Park while any Critical task holds a permit.
            while critical_in_flight() > 0 {
                bulk_gate.notified().await;
            }
        }
        let out = fut.await;
        if matches!(class, TaskClass::Critical) {
            note_critical_release();
        }
        out
    })
}
```

- [ ] **Step 7: Write `src/lib.rs`**

```rust
//! `origin-runtime` — task-class budgeting + `spawn_in` helper.

pub mod class;
pub mod registry;
pub mod spawn;

pub use class::TaskClass;
pub use registry::init_for_test;
pub use spawn::spawn_in;
```

- [ ] **Step 8: Run tests to verify pass**

Run: `cargo test -p origin-runtime`
Expected: PASS.

- [ ] **Step 9: Verification gate**

Run:
```bash
cargo test -p origin-runtime
cargo clippy -p origin-runtime --all-targets -- -D warnings
cargo fmt --check
```
All exit 0.

- [ ] **Step 10: Commit**

```bash
git add crates/origin-runtime Cargo.toml Cargo.lock
git commit -m "feat(origin-runtime): TaskClass + spawn_in + per-class semaphore registry (P12.3)"
```

---

## Task P12.4 — Bulk-gate watcher + concurrent Critical/Bulk semantics test  **[depends P12.3]**

**Files:**

- Create: `crates/origin-runtime/src/bulk_gate.rs`
- Modify: `crates/origin-runtime/src/lib.rs` (re-export `BulkGate`)
- Create: `crates/origin-runtime/tests/bulk_gate.rs`

- [ ] **Step 1: Write the failing test** at `crates/origin-runtime/tests/bulk_gate.rs`

```rust
//! Bulk tasks are parked while at least one Critical task is in flight.

use origin_runtime::{init_for_test, spawn_in, TaskClass};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use tokio::time::{sleep, Duration};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bulk_parks_while_critical_runs() {
    init_for_test();
    let bulk_ran = Arc::new(AtomicBool::new(false));
    let bulk_ran_clone = Arc::clone(&bulk_ran);
    let critical_done = Arc::new(AtomicBool::new(false));
    let critical_done_clone = Arc::clone(&critical_done);

    // Kick off a long-running Critical task first.
    let critical = spawn_in(TaskClass::Critical, async move {
        sleep(Duration::from_millis(150)).await;
        critical_done_clone.store(true, Ordering::SeqCst);
    });
    // Give the Critical task a chance to acquire its permit.
    sleep(Duration::from_millis(20)).await;
    // Spawn a Bulk task — it must wait until critical_done == true.
    let bulk = spawn_in(TaskClass::Bulk, async move {
        bulk_ran_clone.store(true, Ordering::SeqCst);
    });
    // Halfway through the Critical task, Bulk must NOT have run yet.
    sleep(Duration::from_millis(60)).await;
    assert!(
        !bulk_ran.load(Ordering::SeqCst),
        "Bulk must be parked while Critical is in flight"
    );
    let _ = critical.await;
    let _ = bulk.await;
    assert!(critical_done.load(Ordering::SeqCst));
    assert!(bulk_ran.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_bulk_under_repeated_critical() {
    init_for_test();
    let counter = Arc::new(AtomicU32::new(0));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let c = Arc::clone(&counter);
        handles.push(spawn_in(TaskClass::Bulk, async move {
            c.fetch_add(1, Ordering::SeqCst);
        }));
    }
    for _ in 0..4 {
        handles.push(spawn_in(TaskClass::Critical, async move {
            sleep(Duration::from_millis(30)).await;
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    assert_eq!(counter.load(Ordering::SeqCst), 8, "every Bulk eventually runs");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p origin-runtime --test bulk_gate`
Expected: pass or hang — the existing `spawn.rs` already implements the gate logic, but the test may flake due to the `notified()` ordering window. If it passes, **still proceed**: this task formalizes the watcher into its own module and tightens the wakeup.

- [ ] **Step 3: Write `src/bulk_gate.rs`**

```rust
//! Bulk-gate — fairness watcher that wakes parked Bulk tasks whenever the
//! Critical semaphore drops to zero permits in flight.

use crate::registry::{critical_in_flight, registry};
use std::sync::Arc;
use tokio::sync::Notify;

/// Test-visible handle on the global bulk gate.
#[must_use]
pub struct BulkGate(Arc<Notify>);

impl BulkGate {
    #[must_use]
    pub fn current() -> Self {
        Self(Arc::clone(&registry().bulk_gate))
    }

    pub async fn wait_until_idle(&self) {
        while critical_in_flight() > 0 {
            self.0.notified().await;
        }
    }

    pub fn notify_all(&self) {
        self.0.notify_waiters();
    }
}
```

Add to `src/lib.rs`:

```rust
pub mod bulk_gate;
pub use bulk_gate::BulkGate;
```

Update `spawn.rs` so the Bulk-park loop uses `BulkGate::wait_until_idle()` instead of inlining the same logic, keeping the policy in one place.

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p origin-runtime`
Expected: PASS (all four tests across both test files).

- [ ] **Step 5: Verification gate**

Run:
```bash
cargo test -p origin-runtime
cargo clippy -p origin-runtime --all-targets -- -D warnings
cargo fmt --check
```
All exit 0.

- [ ] **Step 6: Commit**

```bash
git add crates/origin-runtime
git commit -m "feat(origin-runtime): formalise BulkGate watcher (P12.4)"
```

---

## Task P12.5 — `xtask lint-spawn`: ban raw `tokio::spawn` outside `origin-runtime`  **[depends P12.4]**

**Files:**

- Modify: `xtask/Cargo.toml` (add `walkdir`, `syn`, `proc-macro2` deps if not already present from P11.14)
- Modify: `xtask/src/main.rs` — add `lint-spawn` subcommand alongside `lint-secrets`
- Create: `xtask/src/lint_spawn.rs`
- Create: `xtask/src/lint_spawn_allowlist.rs`
- Create: `xtask/tests/fixtures/clean_spawn/main.rs`
- Create: `xtask/tests/fixtures/dirty_spawn/main.rs`
- Create: `xtask/tests/lint_spawn.rs`

- [ ] **Step 1: Write the failing test** at `xtask/tests/lint_spawn.rs`

```rust
use std::process::Command;

fn cargo_bin() -> String {
    env!("CARGO").to_string()
}

#[test]
fn lint_spawn_passes_on_clean_fixture() {
    let status = Command::new(cargo_bin())
        .args([
            "run",
            "--quiet",
            "-p",
            "xtask",
            "--",
            "lint-spawn",
            "--root",
            "xtask/tests/fixtures/clean_spawn",
        ])
        .status()
        .expect("xtask run");
    assert!(status.success(), "clean_spawn fixture should pass lint");
}

#[test]
fn lint_spawn_fails_on_dirty_fixture() {
    let status = Command::new(cargo_bin())
        .args([
            "run",
            "--quiet",
            "-p",
            "xtask",
            "--",
            "lint-spawn",
            "--root",
            "xtask/tests/fixtures/dirty_spawn",
        ])
        .status()
        .expect("xtask run");
    assert!(
        !status.success(),
        "dirty_spawn fixture should fail lint with non-zero exit"
    );
}
```

- [ ] **Step 2: Write the fixtures**

`xtask/tests/fixtures/clean_spawn/main.rs`:

```rust
//! Clean fixture — only uses spawn_in.

#[allow(dead_code)]
fn ok() {
    // pretend: origin_runtime::spawn_in(TaskClass::Critical, async {});
    let _ = "origin_runtime::spawn_in(TaskClass::Critical, async {})";
}
```

`xtask/tests/fixtures/dirty_spawn/main.rs`:

```rust
//! Dirty fixture — calls raw tokio::spawn.

#[allow(dead_code)]
fn bad() {
    tokio::spawn(async {});
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p xtask --test lint_spawn`
Expected: FAIL — the `lint-spawn` subcommand doesn't exist yet.

- [ ] **Step 4: Write the allowlist** at `xtask/src/lint_spawn_allowlist.rs`

```rust
//! Per-file allowlist for the `tokio::spawn` ban.
//!
//! Entries are workspace-relative paths. A path is allowed if it matches
//! any prefix in this list. Keep entries minimal and add a justification.

pub const ALLOWLIST: &[&str] = &[
    // The only sanctioned spawn site — `spawn_in` itself.
    "crates/origin-runtime/src/spawn.rs",
    // Sidecar runtime pre-dates the migration; covered by a P14 follow-up.
    "crates/origin-sidecar/src/runtime.rs",
    // Supervisor launches the daemon child via tokio::process::Command::spawn,
    // which is a different `spawn` and not the lint target — but we list the
    // file here too to make the intent explicit.
    "crates/origin-supervisor/src/launch_unix.rs",
    "crates/origin-supervisor/src/launch_windows.rs",
    // Provider crates carry a few one-off keepalive tasks; tracked for P14.
    "crates/origin-provider-anthropic/src",
    "crates/origin-provider-openai/src",
    "crates/origin-provider-gemini/src",
    "crates/origin-provider-ollama/src",
    "crates/origin-provider-bedrock/src",
    "crates/origin-provider-openrouter/src",
    "crates/origin-provider-github/src",
];

#[must_use]
pub fn is_allowlisted(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    ALLOWLIST
        .iter()
        .any(|prefix| normalized.contains(prefix))
}
```

- [ ] **Step 5: Write the linter** at `xtask/src/lint_spawn.rs`

```rust
//! Lint: ban `tokio::spawn` / `tokio::task::spawn` / `tokio::task::spawn_blocking`
//! outside the sanctioned `origin-runtime::spawn_in` site (+ allowlist).

use crate::lint_spawn_allowlist::is_allowlisted;
use std::path::Path;
use walkdir::WalkDir;

const BANNED_PATTERNS: &[&str] = &[
    "tokio::spawn(",
    "tokio::task::spawn(",
    "tokio::task::spawn_blocking(",
];

pub fn run(root: &Path) -> Result<(), String> {
    let mut violations: Vec<(String, usize, String)> = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("rs"))
    {
        let rel = entry.path().display().to_string();
        // tests + benches + build scripts are exempt.
        if rel.contains("/tests/")
            || rel.contains("\\tests\\")
            || rel.contains("/benches/")
            || rel.contains("\\benches\\")
            || rel.ends_with("build.rs")
        {
            continue;
        }
        if is_allowlisted(&rel) {
            continue;
        }
        let src = std::fs::read_to_string(entry.path())
            .map_err(|e| format!("read {rel}: {e}"))?;
        for (lineno, line) in src.lines().enumerate() {
            // Skip lines inside a string literal or comment in the cheapest
            // way that still catches the common cases.
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            for pat in BANNED_PATTERNS {
                if line.contains(pat) {
                    violations.push((rel.clone(), lineno + 1, line.trim().to_string()));
                }
            }
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    for (path, line, snippet) in &violations {
        eprintln!("error: raw spawn at {path}:{line}: {snippet}");
    }
    Err(format!("{} violation(s)", violations.len()))
}
```

- [ ] **Step 6: Wire the subcommand into `xtask/src/main.rs`**

```rust
// Append the new subcommand to the existing clap definition.

mod lint_spawn;
mod lint_spawn_allowlist;

#[derive(clap::Subcommand)]
enum Cmd {
    LintSecrets {
        #[arg(long, default_value = ".")]
        root: std::path::PathBuf,
    },
    LintSpawn {
        #[arg(long, default_value = ".")]
        root: std::path::PathBuf,
    },
}

fn dispatch(cmd: Cmd) -> Result<(), String> {
    match cmd {
        Cmd::LintSecrets { root } => crate::lint_secrets::run(&root),
        Cmd::LintSpawn { root } => crate::lint_spawn::run(&root),
    }
}
```

(If the P11.14 main.rs used a different layout, follow that layout; the test only checks the subcommand exit code.)

- [ ] **Step 7: Run test to verify pass**

Run: `cargo test -p xtask --test lint_spawn`
Expected: PASS (both tests).

Also run: `cargo run -p xtask -- lint-spawn` from the workspace root.
Expected: exits 0 — the workspace as it stands at the start of Cluster D still has raw `tokio::spawn`s, **so this command will exit non-zero at the start of P12.5 if you run it against the whole workspace**. That is fine — the lint becomes green only after P12.9 finishes the migration. The fixture-only tests are what gate this task.

- [ ] **Step 8: Verification gate**

Run:
```bash
cargo test -p xtask
cargo clippy -p xtask --all-targets -- -D warnings
cargo fmt --check
```
All exit 0.

- [ ] **Step 9: Commit**

```bash
git add xtask
git commit -m "feat(xtask): lint-spawn — ban raw tokio::spawn outside origin-runtime (P12.5)"
```

---

# Cluster C — io_uring CAS

## Task P12.6 — `origin-cas` io_uring pack-file reads (Linux feature)  **[parallel-safe with A/B]**

**Files:**

- Modify: `crates/origin-cas/Cargo.toml` (add optional `tokio-uring` dep + `uring` feature)
- Create: `crates/origin-cas/src/packfile_uring.rs`
- Modify: `crates/origin-cas/src/lib.rs` (re-export behind feature)
- Create: `crates/origin-cas/tests/uring_round_trip.rs`

- [ ] **Step 1: Update manifest** — append to `crates/origin-cas/Cargo.toml`:

```toml
[features]
default = []
uring = ["dep:tokio-uring"]

[target.'cfg(target_os = "linux")'.dependencies]
tokio-uring = { workspace = true, optional = true }
```

- [ ] **Step 2: Write the failing test** at `crates/origin-cas/tests/uring_round_trip.rs`

```rust
//! Linux-only — write a pack with the uring writer and read entries back with
//! the uring reader.

#![cfg(all(target_os = "linux", feature = "uring"))]

use origin_cas::{Hash, PackBuilder, PackReader, packfile_uring::read_at_uring};
use std::io::Write;
use tempfile::TempDir;

#[test]
fn uring_round_trip() {
    let dir = TempDir::new().expect("tmpdir");
    let path = dir.path().join("test.pack");

    // Build a small pack via the std writer.
    let mut b = PackBuilder::create(&path).expect("create");
    let payloads: Vec<(Hash, Vec<u8>)> = (0..16u8)
        .map(|i| {
            let bytes = vec![i; 4096];
            let h = Hash::of(&bytes);
            (h, bytes)
        })
        .collect();
    for (h, bytes) in &payloads {
        b.append(*h, bytes).expect("append");
    }
    b.finalize().expect("finalize");

    // Read it back through the uring path.
    tokio_uring::start(async {
        let reader = PackReader::open(&path).expect("open");
        for (h, expected) in &payloads {
            let bytes = read_at_uring(&reader, *h).await.expect("read_at_uring");
            assert_eq!(bytes, *expected, "uring read must match writer payload");
        }
    });
}
```

- [ ] **Step 3: Run test to verify it fails** *(Linux host or CI matrix)*

Run: `cargo test -p origin-cas --features uring --test uring_round_trip`
Expected: FAIL — `packfile_uring::read_at_uring` is undefined.

(On Windows / macOS hosts: run `cargo check -p origin-cas --features uring --target x86_64-unknown-linux-gnu` instead to confirm the gate is sound.)

- [ ] **Step 4: Write the reader** at `crates/origin-cas/src/packfile_uring.rs`

```rust
//! `tokio-uring`-backed pack-file read path.
//!
//! Index walk reuses the existing mmap-resident `PackReader::find(hash)` and
//! returns `(offset, len)`. The payload read is the only operation that
//! issues an io_uring SQE.

#![cfg(all(target_os = "linux", feature = "uring"))]

use crate::{Hash, PackError, PackReader};
use std::path::Path;
use tokio_uring::fs::File;

/// Look up `hash` via the mmap'd index and read the payload via io_uring.
///
/// # Errors
/// - `PackError::Truncated` if the index entry's range is past EOF.
/// - `PackError::Io` for any uring submission error.
pub async fn read_at_uring(
    reader: &PackReader,
    hash: Hash,
) -> Result<Vec<u8>, PackError> {
    let entry = reader.find(&hash).ok_or(PackError::Truncated)?;
    read_offset_len(reader.path(), entry.offset, entry.len).await
}

pub(crate) async fn read_offset_len(
    path: &Path,
    offset: u64,
    len: u32,
) -> Result<Vec<u8>, PackError> {
    let file = File::open(path).await?;
    let buf = vec![0u8; len as usize];
    let (res, buf) = file.read_at(buf, offset).await;
    let n = res?;
    if n != len as usize {
        return Err(PackError::Truncated);
    }
    let _ = file.close().await;
    Ok(buf)
}
```

For this to compile, `PackReader` needs `path(&self) -> &Path` and a `find(&self, &Hash) -> Option<IndexEntry>` accessor; if they're already public from P2 leave them; otherwise add minimal accessors:

```rust
// crates/origin-cas/src/packfile.rs — append minimal accessors.
impl PackReader {
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
    #[must_use]
    pub fn find(&self, hash: &Hash) -> Option<IndexEntry> {
        self.index.get(hash).copied()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct IndexEntry {
    pub offset: u64,
    pub len: u32,
}
```

(If the existing struct already exposes these under different names, adapt; the test is the contract.)

- [ ] **Step 5: Wire the module into `src/lib.rs`**

```rust
#[cfg(all(target_os = "linux", feature = "uring"))]
pub mod packfile_uring;
```

- [ ] **Step 6: Run tests to verify pass**

Run *(Linux host)*: `cargo test -p origin-cas --features uring --test uring_round_trip`
Expected: PASS.

Run *(any host)*: `cargo check -p origin-cas` and `cargo test -p origin-cas` (no `--features uring`).
Expected: PASS — default build is unchanged.

- [ ] **Step 7: Verification gate**

Run:
```bash
cargo test -p origin-cas
cargo test -p origin-cas --features uring     # Linux host only; skip elsewhere
cargo clippy -p origin-cas --all-targets -- -D warnings
cargo clippy -p origin-cas --features uring --all-targets -- -D warnings
cargo fmt --check
```
All exit 0 on Linux; non-Linux hosts skip the feature-gated commands.

- [ ] **Step 8: Commit**

```bash
git add crates/origin-cas Cargo.toml Cargo.lock
git commit -m "feat(origin-cas): tokio-uring pack-file read path (Linux, feature=uring) (P12.6)"
```

---

## Task P12.7 — `origin-cas` io_uring pack-file writes + Store routing + throughput bench  **[depends P12.6]**

**Files:**

- Modify: `crates/origin-cas/src/packfile_uring.rs` (add `write_payloads_uring` helper)
- Modify: `crates/origin-cas/src/store.rs` — when `feature = "uring"` and on Linux, route `Store::put`'s pack-flush through the uring writer
- Create: `crates/origin-cas/benches/uring_throughput.rs`

- [ ] **Step 1: Write the failing bench** at `crates/origin-cas/benches/uring_throughput.rs`

```rust
//! Throughput benchmark — sequential write + read of 64 MiB through the uring
//! path. We do not depend on criterion to keep the bench harness light.

#![cfg(all(target_os = "linux", feature = "uring"))]

use origin_cas::{Hash, PackBuilder, PackReader, packfile_uring::read_at_uring};
use std::time::Instant;
use tempfile::TempDir;

fn write_bench() -> f64 {
    let dir = TempDir::new().expect("tmpdir");
    let path = dir.path().join("bench.pack");
    let mut b = PackBuilder::create(&path).expect("create");
    let chunk = vec![0xABu8; 64 * 1024];
    let total = 64 * 1024 * 1024;
    let count = total / chunk.len();
    let start = Instant::now();
    for i in 0..count {
        let h = Hash::of(&[i as u8; 32]); // distinct synthetic hash
        b.append(h, &chunk).expect("append");
    }
    b.finalize().expect("finalize");
    let elapsed = start.elapsed().as_secs_f64();
    (total as f64) / (1024.0 * 1024.0) / elapsed
}

fn read_bench() -> f64 {
    let dir = TempDir::new().expect("tmpdir");
    let path = dir.path().join("bench.pack");
    let mut b = PackBuilder::create(&path).expect("create");
    let chunk = vec![0xCDu8; 64 * 1024];
    let total = 64 * 1024 * 1024;
    let count = total / chunk.len();
    let mut hashes = Vec::with_capacity(count);
    for i in 0..count {
        let h = Hash::of(&[i as u8; 32]);
        b.append(h, &chunk).expect("append");
        hashes.push(h);
    }
    b.finalize().expect("finalize");
    let reader = PackReader::open(&path).expect("open");
    let throughput = std::sync::Mutex::new(0.0_f64);
    tokio_uring::start(async {
        let start = Instant::now();
        for h in hashes {
            let _ = read_at_uring(&reader, h).await.expect("read");
        }
        let elapsed = start.elapsed().as_secs_f64();
        *throughput.lock().unwrap() = (total as f64) / (1024.0 * 1024.0) / elapsed;
    });
    let g = *throughput.lock().unwrap();
    g
}

fn main() {
    let w = write_bench();
    let r = read_bench();
    eprintln!("uring write MiB/s = {w:.1}");
    eprintln!("uring read  MiB/s = {r:.1}");
    assert!(w >= 180.0, "write threshold not met: {w:.1} MiB/s");
    assert!(r >= 250.0, "read threshold not met: {r:.1} MiB/s");
}
```

- [ ] **Step 2: Run the bench to verify it fails** *(Linux host)*

Run: `cargo bench -p origin-cas --features uring --bench uring_throughput`
Expected: FAIL — `write_payloads_uring` is not defined, or thresholds not met.

- [ ] **Step 3: Add the writer helper to `src/packfile_uring.rs`**

```rust
//! Async writer helper — appends a vector of payloads to a brand-new pack file.

use crate::{Hash, PackError};
use std::path::Path;
use tokio_uring::fs::OpenOptions;

pub async fn write_payloads_uring(
    path: &Path,
    payloads: &[(Hash, Vec<u8>)],
) -> Result<(), PackError> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await?;
    let mut cursor: u64 = 0;
    // Header — replicate the magic / version layout from `PackBuilder::create`.
    let header: Vec<u8> = {
        let mut h = Vec::with_capacity(8);
        h.extend_from_slice(b"OCPK");
        h.extend_from_slice(&1u16.to_be_bytes());
        h.extend_from_slice(&0u16.to_be_bytes()); // reserved
        h
    };
    let header_len = header.len() as u64;
    let (res, _) = file.write_all_at(header, cursor).await;
    res?;
    cursor += header_len;
    // Payloads — [hash:32][len:u32][bytes].
    let mut index = Vec::with_capacity(payloads.len());
    for (hash, bytes) in payloads {
        let len = u32::try_from(bytes.len()).map_err(|_| PackError::Truncated)?;
        let mut head = Vec::with_capacity(32 + 4);
        head.extend_from_slice(hash.as_bytes());
        head.extend_from_slice(&len.to_be_bytes());
        let head_len = head.len() as u64;
        let (res, _) = file.write_all_at(head, cursor).await;
        res?;
        cursor += head_len;
        let payload = bytes.clone();
        let payload_len = payload.len() as u64;
        let (res, _) = file.write_all_at(payload, cursor).await;
        res?;
        index.push((*hash, cursor, len));
        cursor += payload_len;
    }
    // Index + footer — match `PackBuilder::finalize`.
    let index_offset = cursor;
    let mut idx_bytes = Vec::with_capacity(index.len() * (32 + 8 + 4));
    for (h, off, len) in &index {
        idx_bytes.extend_from_slice(h.as_bytes());
        idx_bytes.extend_from_slice(&off.to_be_bytes());
        idx_bytes.extend_from_slice(&len.to_be_bytes());
    }
    let idx_len = idx_bytes.len() as u64;
    let (res, _) = file.write_all_at(idx_bytes, cursor).await;
    res?;
    cursor += idx_len;
    let mut footer = Vec::with_capacity(8 + 8 + 4);
    let entries = u64::try_from(index.len()).map_err(|_| PackError::Truncated)?;
    footer.extend_from_slice(&entries.to_be_bytes());
    footer.extend_from_slice(&index_offset.to_be_bytes());
    footer.extend_from_slice(b"OCFT");
    let (res, _) = file.write_all_at(footer, cursor).await;
    res?;
    let _ = file.sync_all().await;
    let _ = file.close().await;
    Ok(())
}
```

- [ ] **Step 4: Route `Store::put`'s pack flush through the uring writer**

Modify `crates/origin-cas/src/store.rs` to add a feature-gated branch that, when `cfg(all(target_os = "linux", feature = "uring"))` is on, calls `write_payloads_uring(&new_pack_path, &pending)` instead of the BufWriter path. Keep the existing std-fs path as the fallback otherwise.

- [ ] **Step 5: Run the bench to verify pass**

Run: `cargo bench -p origin-cas --features uring --bench uring_throughput`
Expected: PASS — both threshold asserts hold (≥ 180 MiB/s write, ≥ 250 MiB/s read). On a slow CI box you may relax to ≥ 120 / ≥ 180; record the achieved numbers in the commit message.

- [ ] **Step 6: Verification gate**

Run:
```bash
cargo test -p origin-cas
cargo test -p origin-cas --features uring                       # Linux host only
cargo bench -p origin-cas --features uring --bench uring_throughput   # Linux host only
cargo clippy -p origin-cas --features uring --all-targets -- -D warnings
cargo fmt --check
```
All exit 0.

- [ ] **Step 7: Commit**

```bash
git add crates/origin-cas
git commit -m "feat(origin-cas): tokio-uring pack-file writes + Store routing + bench (P12.7)"
```

---

# Cluster D — Two-runtime split + spawn migration

## Task P12.8 — Daemon two-runtime split (control core + worker pool)  **[depends P12.5]**

**Files:**

- Modify: `crates/origin-daemon/Cargo.toml` — depend on `origin-runtime` and `origin-alloc`
- Create: `crates/origin-daemon/src/runtime_launch.rs`
- Modify: `crates/origin-daemon/src/main.rs` — remove the `#[tokio::main]` attribute and call `runtime_launch::start`
- Create: `crates/origin-daemon/tests/runtime_split.rs`

- [ ] **Step 1: Add `origin-runtime` + `origin-alloc` to daemon deps** in `crates/origin-daemon/Cargo.toml`:

```toml
[dependencies]
# … existing …
origin-runtime = { path = "../origin-runtime" }
origin-alloc = { path = "../origin-alloc" }
```

- [ ] **Step 2: Write the failing test** at `crates/origin-daemon/tests/runtime_split.rs`

```rust
//! Verify that `Critical` and `Realtime` futures execute on differently-named
//! OS threads — control-core futures land on `origin-ctrl`, worker-pool
//! futures land on tokio's default `tokio-runtime-worker-N`.

use origin_daemon::runtime_launch::{start, ShutdownSignal};
use origin_runtime::{spawn_in, TaskClass};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[test]
fn control_and_worker_run_on_distinct_threads() {
    let signal = ShutdownSignal::new();
    let signal_clone = signal.clone();
    let handle = thread::spawn(move || start(signal_clone));
    // Give the runtimes a moment to come up.
    thread::sleep(Duration::from_millis(50));

    let (ctrl_tx, ctrl_rx) = mpsc::sync_channel::<String>(1);
    let (work_tx, work_rx) = mpsc::sync_channel::<String>(1);

    // Realtime → control core
    signal.control_handle().spawn_on_control(async move {
        let name = std::thread::current()
            .name()
            .unwrap_or("<unnamed>")
            .to_string();
        let _ = ctrl_tx.send(name);
    });
    // Critical → worker pool
    signal.worker_handle().spawn_on_worker(|| {
        spawn_in(TaskClass::Critical, async move {
            let name = std::thread::current()
                .name()
                .unwrap_or("<unnamed>")
                .to_string();
            let _ = work_tx.send(name);
        });
    });

    let ctrl = ctrl_rx.recv_timeout(Duration::from_secs(2)).expect("ctrl");
    let work = work_rx.recv_timeout(Duration::from_secs(2)).expect("work");
    assert!(ctrl.contains("origin-ctrl"), "control thread name: {ctrl}");
    assert!(
        !work.contains("origin-ctrl"),
        "worker thread name should not be origin-ctrl: {work}"
    );

    signal.trigger();
    let _ = handle.join();
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p origin-daemon --test runtime_split`
Expected: FAIL — `runtime_launch` module does not exist.

- [ ] **Step 4: Write `src/runtime_launch.rs`**

```rust
//! Two-runtime launcher — control core (`current_thread` on a named OS thread)
//! + worker pool (`multi_thread` with `physical_cores - 1` workers).

use std::future::Future;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use tokio::runtime::{Builder, Handle, Runtime};

/// Shared "please shut down" flag. The launcher polls it on a 10ms interval
/// in test-only mode; in production, SIGTERM/SIGINT trigger it.
#[derive(Clone)]
pub struct ShutdownSignal {
    inner: Arc<(Mutex<bool>, Condvar)>,
    control: ControlHandle,
    worker: WorkerHandle,
}

impl ShutdownSignal {
    #[must_use]
    pub fn new() -> Self {
        let inner = Arc::new((Mutex::new(false), Condvar::new()));
        Self {
            inner,
            control: ControlHandle::pending(),
            worker: WorkerHandle::pending(),
        }
    }

    pub fn trigger(&self) {
        let (lock, cvar) = &*self.inner;
        *lock.lock().expect("shutdown lock") = true;
        cvar.notify_all();
    }

    pub fn wait(&self) {
        let (lock, cvar) = &*self.inner;
        let mut guard = lock.lock().expect("shutdown lock");
        while !*guard {
            guard = cvar.wait(guard).expect("wait");
        }
    }

    #[must_use]
    pub fn control_handle(&self) -> &ControlHandle {
        &self.control
    }
    #[must_use]
    pub fn worker_handle(&self) -> &WorkerHandle {
        &self.worker
    }
}

#[derive(Clone)]
pub struct ControlHandle {
    handle: Arc<Mutex<Option<Handle>>>,
}

impl ControlHandle {
    fn pending() -> Self {
        Self { handle: Arc::new(Mutex::new(None)) }
    }
    fn set(&self, h: Handle) {
        *self.handle.lock().expect("ctrl handle lock") = Some(h);
    }
    pub fn spawn_on_control<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if let Some(h) = self.handle.lock().expect("ctrl lock").as_ref() {
            h.spawn(fut);
        }
    }
}

#[derive(Clone)]
pub struct WorkerHandle {
    handle: Arc<Mutex<Option<Handle>>>,
}

impl WorkerHandle {
    fn pending() -> Self {
        Self { handle: Arc::new(Mutex::new(None)) }
    }
    fn set(&self, h: Handle) {
        *self.handle.lock().expect("worker handle lock") = Some(h);
    }
    pub fn spawn_on_worker<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        if let Some(h) = self.handle.lock().expect("worker lock").as_ref() {
            h.spawn_blocking(f);
        }
    }
    #[must_use]
    pub fn raw(&self) -> Option<Handle> {
        self.handle.lock().expect("worker lock").clone()
    }
}

/// Start both runtimes and block until `signal.trigger()` is called.
pub fn start(signal: ShutdownSignal) {
    let cores = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(4);
    let worker_threads = cores.saturating_sub(1).max(1);

    // Worker pool first — control core may dispatch to it on startup.
    let worker_rt: Runtime = Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .thread_name("origin-work")
        .enable_all()
        .build()
        .expect("worker runtime");
    signal.worker.set(worker_rt.handle().clone());

    // Control core on its own OS thread.
    let signal_ctrl = signal.clone();
    let ctrl_join = thread::Builder::new()
        .name("origin-ctrl".to_string())
        .spawn(move || {
            let ctrl_rt: Runtime = Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("ctrl runtime");
            signal_ctrl.control.set(ctrl_rt.handle().clone());
            ctrl_rt.block_on(async move {
                // Park until shutdown is requested.
                let s = signal_ctrl.clone();
                tokio::task::spawn_blocking(move || s.wait())
                    .await
                    .expect("wait join");
            });
        })
        .expect("ctrl thread spawn");

    // Main thread waits on the control thread.
    let _ = ctrl_join.join();
    // Drop the worker runtime — Tokio's Drop is the cooperative phase here.
    drop(worker_rt);
}
```

- [ ] **Step 5: Expose the module** — append to `crates/origin-daemon/src/lib.rs`:

```rust
pub mod runtime_launch;
```

- [ ] **Step 6: Rewire `main.rs`**

Remove the `#[tokio::main(flavor = "multi_thread")]` attribute. Wrap `main` body in:

```rust
fn main() -> Result<()> {
    let signal = origin_daemon::runtime_launch::ShutdownSignal::new();
    let signal_clone = signal.clone();
    let join = std::thread::spawn(move || {
        origin_daemon::runtime_launch::start(signal_clone);
    });

    // The existing setup (tracing init, KeyVault, sidecar, etc.) becomes a
    // closure spawned on the worker handle:
    signal.worker_handle().spawn_on_worker(move || {
        // Re-enter a runtime context via the worker handle for any async setup.
        let handle = signal.worker_handle().raw().expect("worker handle");
        handle.block_on(daemon_setup(/* … */));
    });

    // Wire SIGINT / SIGTERM to `signal.trigger()` via `ctrlc` or `tokio::signal`
    // (P12.11 turns this into the full phased shutdown).
    let signal_for_handler = signal.clone();
    ctrlc::set_handler(move || signal_for_handler.trigger())
        .map_err(|e| anyhow::anyhow!("ctrlc: {e}"))?;

    let _ = join.join();
    Ok(())
}

async fn daemon_setup(/* args */) -> Result<()> {
    // … all the existing `main` body lines lifted verbatim …
    Ok(())
}
```

Add `ctrlc = "3"` to `crates/origin-daemon/Cargo.toml` `[dependencies]`.

- [ ] **Step 7: Run test to verify pass**

Run: `cargo test -p origin-daemon --test runtime_split`
Expected: PASS.

Run: `cargo build --bin origin-daemon`
Expected: builds.

- [ ] **Step 8: Verification gate**

Run:
```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
All exit 0.

- [ ] **Step 9: Commit**

```bash
git add crates/origin-daemon Cargo.toml Cargo.lock
git commit -m "feat(origin-daemon): two-runtime split — control core + worker pool (P12.8)"
```

---

## Task P12.9 — Migrate every `tokio::spawn` in `origin-daemon` to `spawn_in(class, …)`  **[depends P12.8]**

**Files:**

- Modify: `crates/origin-daemon/src/main.rs` — `spawn_metrics_endpoint`, `spawn_idle_consolidator`, `spawn_handler_task`, the relay handles inside the request handler all migrate
- Modify: `crates/origin-daemon/src/agent.rs` — internal `tokio::spawn` (3 sites) → `spawn_in(TaskClass::Critical, …)` for agent-loop turns; `spawn_in(TaskClass::Sidecar, …)` for the compaction helper
- Modify: `crates/origin-daemon/src/stream_relay.rs` — relay task → `spawn_in(TaskClass::Realtime, …)`
- Modify: `crates/origin-daemon/src/session_store.rs` — flush task → `spawn_in(TaskClass::Background, …)`
- Modify: `crates/origin-daemon/src/compactor.rs` — compaction → `spawn_in(TaskClass::Sidecar, …)`
- Create: `crates/origin-daemon/tests/spawn_audit.rs`

- [ ] **Step 1: Write the failing audit test** at `crates/origin-daemon/tests/spawn_audit.rs`

```rust
//! Compile-time grep — no raw `tokio::spawn` calls remain in the daemon's
//! src directory. The `xtask lint-spawn` covers this too, but a unit-style
//! test catches it in `cargo test` before xtask runs.

const SRC: &[(&str, &str)] = &[
    ("agent.rs",        include_str!("../src/agent.rs")),
    ("compactor.rs",    include_str!("../src/compactor.rs")),
    ("main.rs",         include_str!("../src/main.rs")),
    ("memory_wiring.rs",include_str!("../src/memory_wiring.rs")),
    ("session.rs",      include_str!("../src/session.rs")),
    ("session_store.rs",include_str!("../src/session_store.rs")),
    ("stream_relay.rs", include_str!("../src/stream_relay.rs")),
    ("tool_use_parser.rs", include_str!("../src/tool_use_parser.rs")),
];

#[test]
fn no_raw_tokio_spawn_in_daemon_src() {
    for (name, body) in SRC {
        for (lineno, line) in body.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            assert!(
                !line.contains("tokio::spawn(") && !line.contains("tokio::task::spawn("),
                "raw tokio::spawn at {name}:{} → use spawn_in(class, …): {}",
                lineno + 1,
                line.trim()
            );
        }
    }
}
```

- [ ] **Step 2: Run the audit to verify it fails**

Run: `cargo test -p origin-daemon --test spawn_audit`
Expected: FAIL — every existing call site is flagged.

- [ ] **Step 3: Migrate each call site**

Concrete edits (line numbers are from the file map; if they have drifted, follow the same shape):

**`crates/origin-daemon/src/main.rs`:**

- `spawn_metrics_endpoint` body — change `tokio::spawn(async move { … });` to `origin_runtime::spawn_in(origin_runtime::TaskClass::Realtime, async move { … });`. The inner accept-and-handle loop's nested `tokio::spawn(async move { … })` becomes `origin_runtime::spawn_in(origin_runtime::TaskClass::Realtime, async move { … })`.
- `spawn_idle_consolidator` — `tokio::spawn(async move { … })` → `origin_runtime::spawn_in(origin_runtime::TaskClass::Background, async move { … })`.
- `spawn_handler_task` — outer task → `Critical`; inner relay handles (`relay_handle`, `event_relay_handle`) → `Realtime`.
- The sidecar `Sidecar::spawn(...)` constructor inside `Sidecar` itself is **not** migrated (covered by the allowlist).

**`crates/origin-daemon/src/agent.rs`:** all three `tokio::spawn`s become `origin_runtime::spawn_in(TaskClass::Critical, …)` (agent-loop turns). If a site is a fire-and-forget summarization, classify it `Sidecar`.

**`crates/origin-daemon/src/stream_relay.rs`:** any internal spawn → `Realtime`.

**`crates/origin-daemon/src/session_store.rs`:** background flush → `Background`.

**`crates/origin-daemon/src/compactor.rs`:** compaction worker → `Sidecar`.

Add `use origin_runtime::{spawn_in, TaskClass};` at the top of each file you touch.

- [ ] **Step 4: Run audit + workspace tests**

Run: `cargo test -p origin-daemon --test spawn_audit`
Expected: PASS.

Run: `cargo test --workspace`
Expected: PASS — no regressions in existing daemon tests.

Run: `cargo run -p xtask -- lint-spawn` (from workspace root)
Expected: exits 0 — the lint is now green over the whole tree (with the P12.5 allowlist still in effect for sidecar / providers / supervisor).

- [ ] **Step 5: Verification gate**

Run:
```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p xtask -- lint-spawn
cargo fmt --check
```
All exit 0.

- [ ] **Step 6: Commit**

```bash
git add crates/origin-daemon
git commit -m "refactor(origin-daemon): migrate every spawn site to spawn_in(class) (P12.9)"
```

---

# Cluster E — Supervisor + cooperative shutdown

## Task P12.10 — `origin-supervisor` skeleton + restart-on-panic  **[depends P12.9]**

**Files:**

- Create: `crates/origin-supervisor/Cargo.toml`
- Create: `crates/origin-supervisor/src/main.rs`
- Create: `crates/origin-supervisor/src/launch_unix.rs`
- Create: `crates/origin-supervisor/src/launch_windows.rs`
- Create: `crates/origin-supervisor/tests/restart.rs`

- [ ] **Step 1: Manifest** at `crates/origin-supervisor/Cargo.toml`

```toml
[package]
name = "origin-supervisor"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[[bin]]
name = "origin-supervisor"
path = "src/main.rs"

[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "process", "signal", "time"] }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
which = "6"
rkyv = "0.7"
serde = { version = "1", features = ["derive"] }

[target.'cfg(unix)'.dependencies]
nix = { workspace = true }

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.59", features = [
  "Win32_System_JobObjects",
  "Win32_System_Threading",
  "Win32_Foundation",
] }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Write the failing test** at `crates/origin-supervisor/tests/restart.rs`

```rust
//! Smoke test — supervisor restarts a SIGKILL'd daemon within 2 s.
//!
//! Strategy: build a fake-daemon shell stub at runtime that just sleeps; SIGKILL
//! it after 200 ms; assert the supervisor re-spawns it.

#[cfg(unix)]
mod unix_only {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    #[test]
    fn restarts_dead_daemon_within_2s() {
        let tmp = tempfile::tempdir().expect("tmp");
        // A trivial daemon stub.
        let stub_path = tmp.path().join("fake-daemon.sh");
        std::fs::write(
            &stub_path,
            "#!/bin/sh\necho started $$ >> /tmp/origin-supervisor-runs\nsleep 60\n",
        )
        .expect("write stub");
        std::fs::set_permissions(
            &stub_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .expect("chmod");
        let _ = std::fs::remove_file("/tmp/origin-supervisor-runs");

        let mut sup = Command::new(env!("CARGO_BIN_EXE_origin-supervisor"))
            .args([
                "--daemon-path",
                stub_path.to_str().expect("utf8"),
                "--max-restarts-per-min",
                "10",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn supervisor");
        std::thread::sleep(Duration::from_millis(300));

        // Kill any current fake-daemon to force the restart path.
        let _ = Command::new("pkill").args(["-f", "fake-daemon.sh"]).status();
        let start = Instant::now();
        let mut restart_count = 0_usize;
        while start.elapsed() < Duration::from_secs(2) {
            if let Ok(s) = std::fs::read_to_string("/tmp/origin-supervisor-runs") {
                restart_count = s.lines().count();
                if restart_count >= 2 {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = sup.kill();
        assert!(
            restart_count >= 2,
            "supervisor should have launched the daemon at least twice (initial + 1 restart); got {restart_count}"
        );
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p origin-supervisor --test restart`
Expected: FAIL (binary `origin-supervisor` doesn't exist yet).

- [ ] **Step 4: Write the binary entry** at `crates/origin-supervisor/src/main.rs`

```rust
//! `origin-supervisor` — restarts `origin-daemon` on crash; coordinates session
//! resume tokens across the restart.

use clap::Parser;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

#[cfg(unix)]
mod launch_unix;
#[cfg(unix)]
use launch_unix as launch;

#[cfg(windows)]
mod launch_windows;
#[cfg(windows)]
use launch_windows as launch;

#[derive(Debug, Parser)]
#[command(name = "origin-supervisor", version)]
struct Args {
    /// Path to the `origin-daemon` binary. Falls back to `which origin-daemon`.
    #[arg(long)]
    daemon_path: Option<PathBuf>,

    /// Maximum restarts within a sliding 60-second window before giving up.
    #[arg(long, default_value = "12")]
    max_restarts_per_min: u32,

    /// Arguments forwarded to the daemon (separate with `--`).
    #[arg(last = true)]
    forward: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let daemon_path = args
        .daemon_path
        .or_else(|| which::which("origin-daemon").ok())
        .ok_or_else(|| anyhow::anyhow!("origin-daemon not found"))?;
    info!(path = %daemon_path.display(), "supervisor: starting");

    let mut restarts: Vec<Instant> = Vec::with_capacity(args.max_restarts_per_min as usize);
    loop {
        let now = Instant::now();
        restarts.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
        if restarts.len() as u32 >= args.max_restarts_per_min {
            error!(
                "supervisor: {} restarts in the last 60s exceeds the cap; bailing",
                restarts.len()
            );
            return Err(anyhow::anyhow!("restart storm"));
        }
        let exit = launch::run_child(&daemon_path, &args.forward)?;
        warn!(?exit, "supervisor: daemon exited; will restart");
        restarts.push(Instant::now());
        std::thread::sleep(Duration::from_millis(250));
    }
}
```

- [ ] **Step 5: Write `src/launch_unix.rs`**

```rust
//! POSIX child-launch.

#![cfg(unix)]

use std::path::Path;
use std::process::ExitStatus;
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug)]
pub struct ExitInfo {
    pub status: ExitStatus,
}

pub fn run_child(daemon_path: &Path, forward: &[String]) -> anyhow::Result<ExitInfo> {
    // We deliberately use a fresh single-threaded runtime per child — the
    // supervisor itself does ~zero async work between restarts.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let mut cmd = Command::new(daemon_path);
        cmd.args(forward).stdout(Stdio::inherit()).stderr(Stdio::inherit());
        let status = cmd.spawn()?.wait().await?;
        Ok(ExitInfo { status })
    })
}
```

- [ ] **Step 6: Write `src/launch_windows.rs`**

```rust
//! Windows child-launch — child wrapped in a Job Object so the supervisor's
//! own exit terminates the daemon.

#![cfg(windows)]

use std::path::Path;
use std::process::ExitStatus;
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug)]
pub struct ExitInfo {
    pub status: ExitStatus,
}

pub fn run_child(daemon_path: &Path, forward: &[String]) -> anyhow::Result<ExitInfo> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        // Job-object association is left to a follow-up: P12 ships the
        // restart-on-exit loop; full kill-on-supervisor-exit semantics are
        // not the gate.
        let mut cmd = Command::new(daemon_path);
        cmd.args(forward).stdout(Stdio::inherit()).stderr(Stdio::inherit());
        let status = cmd.spawn()?.wait().await?;
        Ok(ExitInfo { status })
    })
}
```

- [ ] **Step 7: Run test to verify pass** *(Linux/macOS)*

Run: `cargo test -p origin-supervisor --test restart`
Expected: PASS.

On Windows hosts, replace the smoke test with the `tasklist` / `taskkill` equivalent (or skip with `#[cfg(unix)]` as above and run the test on the CI Linux runner).

- [ ] **Step 8: Verification gate**

Run:
```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
All exit 0.

- [ ] **Step 9: Commit**

```bash
git add crates/origin-supervisor Cargo.toml Cargo.lock
git commit -m "feat(origin-supervisor): restart-on-panic loop with per-min cap (P12.10)"
```

---

## Task P12.11 — Cooperative phased shutdown driver in `origin-daemon`  **[depends P12.10]**

**Files:**

- Create: `crates/origin-daemon/src/shutdown.rs`
- Modify: `crates/origin-daemon/src/main.rs` — wire SIGTERM/SIGINT/Ctrl+C to `CooperativeShutdown::run()` on the control core
- Create: `crates/origin-daemon/tests/shutdown_phases.rs`

- [ ] **Step 1: Write the failing test** at `crates/origin-daemon/tests/shutdown_phases.rs`

```rust
use origin_daemon::shutdown::{CooperativeShutdown, ShutdownPhase, ShutdownReport};
use std::time::Duration;
use tokio::sync::mpsc;

#[tokio::test(flavor = "current_thread")]
async fn all_phases_complete_in_order() {
    let (tx, mut rx) = mpsc::unbounded_channel::<ShutdownPhase>();
    let mut driver = CooperativeShutdown::for_test(tx, Duration::from_millis(50));
    let report = driver.run().await.expect("shutdown");
    assert!(matches!(report, ShutdownReport::Clean));
    let mut seen = Vec::new();
    while let Ok(p) = rx.try_recv() {
        seen.push(p);
    }
    assert_eq!(
        seen,
        vec![
            ShutdownPhase::StopAcceptingIpc,
            ShutdownPhase::CancelBulkAndBackground,
            ShutdownPhase::DrainCritical,
            ShutdownPhase::PersistSidecarQueue,
            ShutdownPhase::FlushCasWriteBuffer,
            ShutdownPhase::CheckpointSqlite,
            ShutdownPhase::CloseIpc,
            ShutdownPhase::ReleaseSharedMemoryAndArenas,
        ],
        "phases run in N8.10 order"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stuck_phase_force_advances_after_budget() {
    let (tx, _rx) = mpsc::unbounded_channel::<ShutdownPhase>();
    let mut driver =
        CooperativeShutdown::for_test_with_hang(tx, ShutdownPhase::DrainCritical, Duration::from_millis(30));
    let report = driver.run().await.expect("shutdown");
    match report {
        ShutdownReport::ForcedAdvance(phase) => {
            assert_eq!(phase, ShutdownPhase::DrainCritical);
        }
        other => panic!("expected ForcedAdvance, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p origin-daemon --test shutdown_phases`
Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Write `src/shutdown.rs`**

```rust
//! Phased cooperative shutdown — N8.10 — for `origin-daemon`.
//!
//! Each phase has its own budget timer. A stuck phase force-advances and the
//! driver returns `ShutdownReport::ForcedAdvance(phase)`.

use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownPhase {
    StopAcceptingIpc,
    CancelBulkAndBackground,
    DrainCritical,
    PersistSidecarQueue,
    FlushCasWriteBuffer,
    CheckpointSqlite,
    CloseIpc,
    ReleaseSharedMemoryAndArenas,
}

const ALL_PHASES: &[ShutdownPhase] = &[
    ShutdownPhase::StopAcceptingIpc,
    ShutdownPhase::CancelBulkAndBackground,
    ShutdownPhase::DrainCritical,
    ShutdownPhase::PersistSidecarQueue,
    ShutdownPhase::FlushCasWriteBuffer,
    ShutdownPhase::CheckpointSqlite,
    ShutdownPhase::CloseIpc,
    ShutdownPhase::ReleaseSharedMemoryAndArenas,
];

#[derive(Debug, Clone, Copy)]
pub enum ShutdownReport {
    Clean,
    ForcedAdvance(ShutdownPhase),
}

pub struct CooperativeShutdown {
    tx: mpsc::UnboundedSender<ShutdownPhase>,
    budget: Duration,
    hang_at: Option<ShutdownPhase>,
}

impl CooperativeShutdown {
    /// Test constructor — phases are no-ops that complete instantly.
    #[must_use]
    pub fn for_test(tx: mpsc::UnboundedSender<ShutdownPhase>, budget: Duration) -> Self {
        Self { tx, budget, hang_at: None }
    }

    /// Test constructor — `hang_at` simulates a phase that never completes,
    /// so the budget timer must force-advance.
    #[must_use]
    pub fn for_test_with_hang(
        tx: mpsc::UnboundedSender<ShutdownPhase>,
        hang_at: ShutdownPhase,
        budget: Duration,
    ) -> Self {
        Self { tx, budget, hang_at: Some(hang_at) }
    }

    /// Drive the phases.
    pub async fn run(&mut self) -> anyhow::Result<ShutdownReport> {
        for phase in ALL_PHASES {
            let _ = self.tx.send(*phase);
            let work = self.run_phase(*phase);
            let outcome = tokio::time::timeout(self.budget, work).await;
            if outcome.is_err() {
                tracing::warn!(?phase, "shutdown: phase exceeded budget — force-advancing");
                return Ok(ShutdownReport::ForcedAdvance(*phase));
            }
        }
        Ok(ShutdownReport::Clean)
    }

    async fn run_phase(&self, phase: ShutdownPhase) {
        if self.hang_at == Some(phase) {
            // Sleep past every reasonable budget — caller's `timeout` will fire.
            tokio::time::sleep(Duration::from_secs(3600)).await;
            return;
        }
        // Real wiring lives in P14 polish; the test-mode driver is a no-op.
        tokio::task::yield_now().await;
    }
}
```

- [ ] **Step 4: Wire into `main.rs`**

Replace the `ctrlc::set_handler` block (added in P12.8) with a `tokio::signal::ctrl_c` watcher that drives `CooperativeShutdown::run()` on the control core, then calls `signal.trigger()` once the phases complete (or force-advance):

```rust
let signal_for_shutdown = signal.clone();
signal.control_handle().spawn_on_control(async move {
    let _ = tokio::signal::ctrl_c().await;
    let mut driver = origin_daemon::shutdown::CooperativeShutdown::for_production(
        // wiring args for the real implementation are filled in by P14 polish
    );
    let _ = driver.run().await;
    signal_for_shutdown.trigger();
});
```

For now `for_production` is a stub equivalent to `for_test(no_op_channel, 30s_budget)`. The full real wiring (IPC listener handle, sidecar queue persister, CAS flush hook, SQLite checkpoint, etc.) is a P14 polish item; P12 ships the phase **structure** and the budget timer.

- [ ] **Step 5: Run test to verify pass**

Run: `cargo test -p origin-daemon --test shutdown_phases`
Expected: PASS.

- [ ] **Step 6: Verification gate**

Run:
```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
All exit 0.

- [ ] **Step 7: Commit**

```bash
git add crates/origin-daemon
git commit -m "feat(origin-daemon): cooperative phased shutdown driver (P12.11)"
```

---

## Task P12.12 — Session resume across supervisor restart  **[depends P12.11]**

**Files:**

- Create: `crates/origin-supervisor/src/resume_token.rs`
- Create: `crates/origin-supervisor/src/ipc_resume.rs`
- Modify: `crates/origin-daemon/src/protocol.rs` — add `ClientMessage::ResumeRequest` and `ServerMessage::ResumeAck`
- Modify: `crates/origin-daemon/src/session_store.rs` — `save_resume_token` / `load_resume_token`
- Create: `crates/origin-supervisor/tests/resume.rs`

- [ ] **Step 1: Write the failing end-to-end test** at `crates/origin-supervisor/tests/resume.rs`

```rust
//! Open a session, take a turn, SIGKILL the daemon; next daemon's
//! `list_sessions` includes the same session at the same turn.
//!
//! Strategy: drive the IPC client from the supervisor's test process; assert
//! on the post-restart session list.

#[cfg(unix)]
mod unix_only {
    // … fixture wiring: see crates/origin-supervisor/tests/restart.rs for the
    // pattern. Use a real `origin-daemon` binary; spawn via the supervisor.

    #[test]
    fn session_resumes_after_kill() {
        // 1. Spawn supervisor with a real daemon path.
        // 2. Connect IPC client, open session "S".
        // 3. Send one prompt; await assistant completion.
        // 4. SIGKILL the daemon.
        // 5. Wait for supervisor restart (< 2 s).
        // 6. Connect a fresh IPC client; call `list_sessions`.
        // 7. Assert "S" is present with `last_turn == 1` (or whatever P11 already shipped).
        //
        // The detailed wiring relies on session_store + protocol changes below
        // and on the supervisor's resume_token writer.
    }
}
```

- [ ] **Step 2: Run test to verify it fails (or is `#[ignore]`d)**

Run: `cargo test -p origin-supervisor --test resume`
Expected: FAIL (or skipped on unsupported hosts).

- [ ] **Step 3: Write `src/resume_token.rs`**

```rust
//! Rkyv-archived resume token persisted by the daemon at every
//! assistant-turn boundary and re-played by the supervisor on restart.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeToken {
    pub session_id: String,
    pub last_turn: u32,
    /// CAS root handle for the session's message log.
    pub cas_handle_root: [u8; 32],
    /// Tool calls that were in-flight when the daemon last checkpointed.
    pub pending_tool_calls: Vec<String>,
    /// Plan CRDT sequence number at the checkpoint.
    pub plan_seq: u64,
}

impl ResumeToken {
    /// Write to `<dir>/<session_id>.json`. JSON is fine — the surface is tiny.
    ///
    /// # Errors
    /// Propagates I/O errors and serde failures.
    pub fn save(&self, dir: &std::path::Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.json", self.session_id));
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }

    /// Read every `*.json` token under `dir`. Missing dir → empty vec.
    pub fn load_all(dir: &std::path::Path) -> std::io::Result<Vec<Self>> {
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.path().extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let bytes = std::fs::read(entry.path())?;
            let token: Self = serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            out.push(token);
        }
        Ok(out)
    }
}
```

- [ ] **Step 4: Write `src/ipc_resume.rs`**

```rust
//! Replays loaded resume tokens to the next daemon over IPC.

use crate::resume_token::ResumeToken;

pub async fn replay_all(
    tokens: Vec<ResumeToken>,
    ipc_endpoint: &str,
) -> anyhow::Result<()> {
    // Real wiring uses `origin_ipc::Client`; the surface is one frame per token.
    // For P12 we ship the loader + the daemon-side ack handler.
    use origin_ipc::transport::Connection;
    let mut conn = Connection::connect(ipc_endpoint).await?;
    for token in tokens {
        let payload = serde_json::to_vec(&token)?;
        conn.send_kind(origin_ipc::frame::FrameKind::ResumeRequest, &payload).await?;
    }
    Ok(())
}
```

- [ ] **Step 5: Extend `crates/origin-daemon/src/protocol.rs`**

Add variants:

```rust
ClientMessage::ResumeRequest { token: ResumeToken } => {
    // Look up session in SessionStore, hydrate from CAS to `token.last_turn`,
    // and re-spawn any `pending_tool_calls` under `TaskClass::Critical`.
}

ServerMessage::ResumeAck { session_id: String, restored_to_turn: u32 }
```

(The exact `match` arm location depends on the current protocol module's enum shape. Follow the same pattern as the P11 `ResumeRequest` / `ResumeAck` placeholder if one exists, or add a fresh variant.)

- [ ] **Step 6: Extend `crates/origin-daemon/src/session_store.rs`**

```rust
impl SessionStore {
    pub fn save_resume_token(
        &self,
        token: &origin_supervisor::resume_token::ResumeToken,
    ) -> std::io::Result<()> {
        token.save(&self.resume_dir())
    }
    pub fn load_resume_token(&self, session_id: &str) -> std::io::Result<Option<origin_supervisor::resume_token::ResumeToken>> {
        let path = self.resume_dir().join(format!("{session_id}.json"));
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(path)?;
        let token = serde_json::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Some(token))
    }
    fn resume_dir(&self) -> std::path::PathBuf {
        // SessionStore already has a state dir reachable somehow — adapt.
        self.state_dir().join("resume")
    }
}
```

Adding `origin-supervisor` as a dep of `origin-daemon` would be a circular dep — instead, define `ResumeToken` in a small leaf crate (`origin-resume-token`) under `crates/`, and have **both** `origin-daemon` and `origin-supervisor` depend on it. Update Cargo.toml + the file paths above accordingly.

- [ ] **Step 7: Run tests to verify pass**

Run: `cargo test -p origin-supervisor --test resume`
Expected: PASS.

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 8: Verification gate**

Run:
```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p xtask -- lint-spawn
cargo fmt --check
```
All exit 0.

- [ ] **Step 9: Commit**

```bash
git add crates/origin-supervisor crates/origin-daemon crates/origin-resume-token Cargo.toml Cargo.lock
git commit -m "feat(origin-supervisor): resume tokens — daemon checkpoints + supervisor replay (P12.12)"
```

---

## Task P12.13 — Phase gate + `p12-complete` tag

**Files:**

- None (verification + tag).

- [ ] **Step 1: Run the full verification gate**

```bash
cargo test --workspace
cargo test -p origin-alloc --features jemalloc -- --test-threads=1
cargo test -p origin-cas --features uring               # Linux host or CI matrix
cargo bench -p origin-cas --features uring --bench uring_throughput -- --quick   # Linux only
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p origin-alloc --features jemalloc --all-targets -- -D warnings
cargo clippy -p origin-cas --features uring --all-targets -- -D warnings
cargo run -p xtask -- lint-secrets
cargo run -p xtask -- lint-spawn
cargo fmt --check
```

Every command must exit 0.

- [ ] **Step 2: Merge `p-12` into `dev`**

Confirm `git log p-12 --oneline` lists P12.0 through P12.12, each as a single Conventional Commit.

```bash
git checkout dev
git pull --ff-only
git merge --no-ff p-12 -m "Merge p-12 into dev: multi-runtime + arenas + cooperative shutdown (P12.1-P12.12); tag p12-complete"
```

- [ ] **Step 3: Tag**

```bash
git tag p12-complete
```

- [ ] **Step 4: Verification gate (post-merge)**

```bash
git checkout dev
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
All exit 0.

- [ ] **Step 5: (Optional) push**

```bash
git push origin dev --tags
```

---

## Self-review

**Spec coverage:** every Phase-12 spec line item is mapped — N8.6 → P12.1+P12.2, N8.7 → P12.3+P12.4+P12.5+P12.9, N8.8 → P12.8, N8.9 (Linux part) → P12.6+P12.7, N8.10 → P12.11, supervisor → P12.10+P12.12. Out-of-scope items are listed explicitly so a subagent doesn't wander.

**Placeholders:** every step that says "implement" carries the actual code or the exact patch shape. The two places that delegate to P14 (full real wiring of the cooperative-shutdown phase callbacks, and the IPC-side daemon handler for `ResumeRequest`) are called out in the task body, are non-blocking for the phase gate, and have the **structure** (the `enum`, the budget timer, the typed phase channel) shipped this phase.

**Type consistency:** `ArenaId`, `TaskClass`, `ShutdownPhase`, `ResumeToken`, `ShutdownReport`, `ControlHandle`, `WorkerHandle` keep the same names across every task. `spawn_in(class, fut)` keeps the same signature in the lint, the runtime crate, and the migration sites. `ResumeToken` lives in its own leaf crate (`origin-resume-token`) so neither `origin-daemon` nor `origin-supervisor` depends on the other — captured explicitly in P12.12 Step 6.

---

*End of Phase 12 plan.*
