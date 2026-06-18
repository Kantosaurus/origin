# origin-supervisor

> Restarts origin-daemon on crash and resumes sessions across restarts.

## Purpose

`origin-supervisor` is the small, always-running parent process that keeps a
single `origin-daemon` alive: it restarts the daemon on crash, replays the
resume tokens needed to bring sessions back, performs the self-dev **binary
swap** when the daemon asks for one, and rolls back to the previous binary if a
freshly-swapped daemon crash-loops. It also carries a pure **lifecycle policy**
that decides when to shed, retire, or hand off sessions under memory and idle
pressure. The crate ships both a library surface (shared with integration tests)
and the `origin-supervisor` binary.

## Public API surface

| Item | Kind | Description |
|------|------|-------------|
| `LifecyclePolicy` | struct | Pure state machine: shed / retire / hand-off decisions. |
| `LifecycleConfig` | struct | Tunable thresholds (idle grace, mem budget, shed ratio); `from_env`. |
| `Decision` / `SessionState` / `AttachState` | enum/struct | Lifecycle outputs and per-session state. |
| `SessionClass` | enum | `Interactive` (never shed) vs `Detached`. |
| `MemReading` | struct | RSS + optional cgroup ratio; `pressure(budget)`. |
| `HandoffKind` / `RetireReason` | enum | Why a session was handed off / retired. |
| `decide_relaunch` | fn | Decide swap / restart-same after the child exits. |
| `RelaunchDecision` | enum | `SwapTo(path)` / `RestartSame` / `Rollback(path)`. |
| `RelaunchManifest` | struct | On-disk daemon→supervisor binary-swap handoff. |
| `load_manifest` / `perform_swap` / `should_rollback` / `watch_outcome` | fn | Relaunch plumbing. |
| `SELFDEV_RELAUNCH_EXIT_CODE` | const | Re-exported sentinel from `origin-selfdev`. |

Module map: `ipc_resume`, `lifecycle`, `relaunch`, `resume_token`.

## Key types

```rust
pub fn decide_relaunch(
    child_exit_code: i32,
    manifest: Option<&RelaunchManifest>,
    new_binary_valid: bool,
) -> RelaunchDecision;

pub enum RelaunchDecision { SwapTo(PathBuf), RestartSame, Rollback(PathBuf) }

pub struct RelaunchManifest {
    pub new_binary_path: PathBuf,
    pub previous_binary_path: PathBuf,
    pub generation: u64,
}

pub enum SessionClass { Interactive, Detached }
pub struct MemReading { pub rss_bytes: u64, pub ratio: Option<f64> }
```

## How it works

**Crash-restart + relaunch.** The supervisor spawns the daemon as a child and
waits. On exit it consults `decide_relaunch`:

```
child exits ──► decide_relaunch(exit_code, manifest, new_binary_valid)
   exit == SELFDEV_RELAUNCH_EXIT_CODE  ∧  manifest present  ∧  new binary valid
        └─► SwapTo(new_binary_path)   then start it, watch_outcome(...)
                                         └─ crash-loops? should_rollback ─► Rollback(previous)
   any other case ─────────────────► RestartSame   (conservative: keep known-good binary)
```

The relaunch logic is pure (path/exit-code arithmetic); the binary's restart
loop drives it with the real child process. The `SELFDEV_RELAUNCH_EXIT_CODE`
sentinel is owned by the leaf crate `origin-selfdev` and re-exported here so both
processes agree on it without a dependency cycle.

**Lifecycle policy.** Once many sessions coexist under one daemon, the
`LifecyclePolicy` folds injected `MemReading`s and a monotonic `now_ms` into
`Decision`s — performing *no* I/O (no real RSS reads, no `Instant::now`, no
process killing). Interactive sessions are sacrosanct; `Detached` ones are shed
first (oldest-idle first) under memory pressure, and retired after a longer
grace. Detached clients are preserved via the existing `ResumeToken`/`ipc_resume`
mechanism so a returning user can re-attach.

## Dependencies & features

- Runtime deps: `origin-resume-token`, `origin-ipc`, `origin-selfdev` (sentinel
  only), `tokio` (process/signal/time), `clap`, `anyhow`, `tracing`, `serde`.
- Platform: `nix` on Unix; `windows-sys` (Job Objects) on Windows for child
  process grouping.
- `#![forbid(unsafe_code)]`. Ships `origin-supervisor` and a test-only
  `selfdev-fixture-daemon` binary.

## Used by

`Grep "origin-supervisor" glob "crates/*/Cargo.toml"` →

- `crates/origin-daemon/Cargo.toml`
- `crates/origin-supervisor/Cargo.toml` (self)

The daemon depends on it to share lifecycle/resume types; in deployment the
supervisor is the parent that launches the daemon binary.

## Testing

`tests/` directory: `restart.rs` (crash-restart loop), `resume.rs` (session
resume across restart), and `relaunch_e2e.rs` (full self-dev relaunch round-trip
driven against the `selfdev-fixture-daemon` fixture via
`CARGO_BIN_EXE_selfdev-fixture-daemon`). The relaunch-sentinel sync test guards
against drift from `origin-selfdev`.

## See also

- [Runtime & concurrency architecture](../architecture/runtime-and-concurrency.md)
- [Agent & sessions subsystem](../subsystems/agent-and-sessions.md)
- [origin-selfdev](./origin-selfdev.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
