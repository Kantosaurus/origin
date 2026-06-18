# Daemon & Supervisor

Operational guide to the **`origin-daemon`** process and the **`origin-supervisor`**
that keeps it alive. This is the runbook for understanding the daemon lifecycle,
how crashes are recovered, how sessions survive a restart, how a graceful
shutdown drains state, and how to inspect or restart the whole thing by hand.

> Cross-links: the two-runtime split is detailed in
> [`../architecture/runtime-and-concurrency.md`](../architecture/runtime-and-concurrency.md);
> traces/logs in [`observability-runbook.md`](./observability-runbook.md);
> install model in [`deployment.md`](./deployment.md).

---

## The two processes

```
origin (CLI/TUI)
   └─ spawns ─▶ origin-supervisor ──spawns/restarts──▶ origin-daemon
                      │                                     │
                      │  resume tokens (replay on restart)  │
                      └──────── local IPC endpoint ─────────┘
```

| Process | Job | Crash behaviour |
|---|---|---|
| `origin-daemon` | Hosts the agent loop, IPC server, CAS, session DB, providers, metrics, traces. | Exits; supervisor restarts it. |
| `origin-supervisor` | Restart-on-crash loop, resume-token replay, self-dev binary swap + rollback. | Bails only on a restart storm (terminal). |

The IPC endpoint is **per-workspace** (a hash of the canonicalized workspace
root); `ORIGIN_SOCK` overrides it. See the endpoint table in
[`deployment.md`](./deployment.md).

---

## Daemon lifecycle

1. **Boot.** `main` installs the parquet-backed tracing layer (its guard owns the
   `origin-trace-drain` thread for the lifetime of `main`) and a human-readable
   text log at `<data>/origin/logs/daemon.log`.
2. **Two-runtime launch.** A launcher thread spins up the **control core**
   (`origin-ctrl`, a `current_thread` runtime: IPC accept loop, renderer ticks,
   event dispatch) and the **worker pool** (`origin-work`, a `multi_thread`
   runtime with `physical_cores - 1` workers: provider HTTP/2, agent turns, tool
   execution). The split isolates the latency-critical control path from
   CPU-heavy work.
3. **Bind & serve.** The daemon binds its IPC endpoint and (optionally) a
   `/metrics` endpoint and an OTLP exporter (see the observability runbook).
4. **Run.** The agent loop processes turns; each assistant-turn boundary
   checkpoints a **resume token** so a crash loses at most the in-flight turn.
5. **Shutdown.** SIGTERM/SIGINT are wired to a shutdown signal; the control task
   drives a **phased cooperative shutdown** (below), then the launcher returns.

`ShutdownSignal::trigger()` is safe to call from any thread or signal handler;
the control runtime's parking task wakes and the launcher unwinds cleanly.

---

## The supervisor: restart on crash

`run_supervisor_loop` is the heart of `origin-supervisor`:

- It (re)spawns the daemon, blocks on the child, and folds each exit into a
  **crash-storm guard**: a sliding **60-second** window of restart timestamps.
- If restarts in that window reach `--max-restarts-per-min` (**default 12**), the
  supervisor logs *"restart storm"* and **bails** — the one terminal condition.
  Otherwise it sleeps 250 ms and respawns, forever.
- The daemon binary is `--daemon-path <p>` or the first `origin-daemon` on
  `PATH`.

```sh
# Run the daemon under the supervisor, forwarding daemon args after `--`:
origin-supervisor --daemon-path ./target/release/origin-daemon \
  --max-restarts-per-min 12 -- --metrics-bind 127.0.0.1:9090
```

A restart storm almost always means a deterministic startup failure (bad config,
unwritable home, a panicking subsystem). Read `daemon.log` and run
`origin doctor` before bumping the cap.

### Self-dev binary swap & rollback

The supervisor also implements live binary replacement for self-development:

| Exit | Manifest present + new binary valid? | Action |
|---|---|---|
| `SELFDEV_RELAUNCH_EXIT_CODE` (86) | yes | **SwapTo**: atomically stage + rename the new binary over the current one, back up the old, arm a rollback watch. |
| `86` | no / invalid | RestartSame (conservative). |
| crash (non-86) while watching a fresh swap | — | Count crashes; **roll back** to the backup if **≥ 2 crashes within 5 s** of the swap; clear the watch if it survives the window. |
| any other | — | RestartSame. |

The swap is atomic (stage a sibling temp file, then `rename`) so a reader sees
either the whole old or whole new binary. A truncated/empty new binary fails
validation and is never swapped in.

---

## Resuming sessions across a restart

The supervisor's stated purpose is not just "restart" but **re-attach live
sessions**. The mechanism is the **resume token** (`origin-resume-token`):

- The daemon checkpoints a `ResumeToken` per session at each assistant-turn
  boundary, written to `<state_dir>/resume/<session_id>.json`.
- A token carries: `session_id`, `last_turn`, `cas_handle_root` (CAS root of the
  message log, so the transcript re-hydrates without re-walking SQLite),
  `pending_tool_calls` (re-spawned as `TaskClass::Critical`), `plan_seq`, an
  optional `/goal` snapshot, and (for shed sessions) `detached_at_unix` +
  `memory_estimate_bytes`.
- Each token is **MAC-wrapped**: `blake3::keyed_hash` over the compact payload,
  with a 32-byte key at `<dir>/.mac-key` (generated on first save via
  `getrandom`, `chmod 0600` on Unix). On load, a missing key, a tampered payload,
  or the legacy bare-JSON format is **rejected** (constant-time compare). This
  closes a code-execution gadget: without the MAC, anyone who could write the
  resume dir could swap `cas_handle_root` and steer the resumed agent.

On every (re)spawn the supervisor kicks off a **detached, best-effort replay**:

1. Enumerate tokens in the resume dir. **No tokens → no-op** (no IPC connection
   even opens — byte-identical to having no detached sessions).
2. Otherwise, connect to the freshly-binding daemon endpoint with bounded retry
   (**40 attempts × 250 ms ≈ 10 s**), then send one `ResumeRequest` per token.
3. Any failure (unreadable dir, tampered token, daemon never reachable) is
   **logged and swallowed** — one bad token must never wedge the restart loop.

### Lifecycle policy: shedding & retirement

Beyond crash-restart, a pure policy (`origin-supervisor::lifecycle`) governs many
coexisting sessions under one daemon. It performs **no I/O** — the daemon injects
a monotonic clock and RSS readings; the policy emits `Decision`s.

| Pressure | Behaviour | Env knob (default) |
|---|---|---|
| **Memory** | Shed background/detached sessions, **oldest-idle first**, never the foreground interactive one, until projected usage drops below the shed threshold. Shed sessions are parked **Detached** (still re-attachable). | `ORIGIN_SUPERVISOR_MEM_BUDGET_MB` (1024), `ORIGIN_SUPERVISOR_MEM_SHED_RATIO` (0.9) |
| **Idle (interactive)** | Retire after the interactive grace. | `ORIGIN_SUPERVISOR_IDLE_GRACE_MS` (300000 = 5 min) |
| **Idle (detached)** | Retire after the longer detached grace; a re-attach resets timers and cancels retirement. | `ORIGIN_SUPERVISOR_DETACHED_GRACE_MS` (1800000 = 30 min) |

The config is sanitized: budget ≥ 1, shed ratio in `(0, 4]`, and
`detached_grace_ms ≥ idle_grace_ms` (a detached session must never be retired
*sooner* than an interactive one).

---

## Graceful shutdown phases & draining

On a shutdown signal the daemon runs a **phased cooperative shutdown**. Each
phase has its own budget timer; a stuck phase **force-advances** and the driver
reports `ForcedAdvance(phase)` (remaining phases are skipped, since they'd block
on the same hung resource). The ordered phases:

| # | Phase | What it does |
|---|---|---|
| 1 | `StopAcceptingIpc` | Stop accepting new IPC connections. |
| 2 | `CancelBulkAndBackground` | Cancel best-effort/background tasks. |
| 3 | `DrainCritical` | Let in-flight critical work finish. |
| 4 | `PersistSidecarQueue` | Drain in-flight sidecar jobs (`Sidecar::shutdown`). |
| 5 | `FlushCasWriteBuffer` | Flush in-memory CAS bytes to disk. |
| 6 | `CheckpointSqlite` | Checkpoint the SQLite WAL. |
| 7 | `CloseIpc` | Close the transport. |
| 8 | `ReleaseSharedMemoryAndArenas` | Release shared memory / arenas. |

A clean run logs `cooperative shutdown complete` with `ShutdownReport::Clean`.
The order is deliberate: stop intake, then cancel cheap work, then preserve
critical work and persist state, then tear down transports and memory last.

---

## Inspecting and restarting

### Is the daemon up?

```sh
origin doctor        # the `daemon` probe reports reachable / not running
```

### Where is its state?

| Artifact | Path |
|---|---|
| Text log (tail this) | `<data>/origin/logs/daemon.log` |
| Trace ring (postmortem) | `<data>/origin/trace/trace-*.parquet` |
| Session DB | `$TMPDIR/origin-<hex>.db` (or `ORIGIN_DB`) |
| CAS root | `$TMPDIR/origin-cas-<hex>/` |
| Resume tokens | `<state_dir>/resume/*.json` + `.mac-key` |
| Control files (stamp/pid) | `<home>/.origin/daemons/<hex>.{stamp,pid}` |
| Self-dev relaunch manifest | `<data>/origin/selfdev/relaunch.json` |

The pid file records the exact daemon/supervisor PIDs spawned for this workspace,
so a restart kills **only** those — never another project's daemon.

### Tail the log live

```sh
tail -f "$HOME/.local/share/origin/logs/daemon.log"   # Linux
# macOS: ~/Library/Application Support/origin/logs/daemon.log
# Windows: %LOCALAPPDATA%\origin\logs\daemon.log
```

Raise verbosity by starting the daemon with `ORIGIN_LOG=debug` (falls back to
`RUST_LOG`, default `info`).

### Restart by hand

```sh
# 1) Identify this workspace's processes (from the pid file), then stop them.
#    A clean SIGTERM lets the phased shutdown drain state:
kill -TERM <daemon-pid>          # Unix
# Windows: taskkill /PID <pid>   (the daemon's own shutdown path runs on close)

# 2) Re-launching `origin` in the workspace re-spawns the daemon (and supervisor).
origin
```

Because sessions are checkpointed, a restart re-attaches detached/in-flight
sessions automatically via resume-token replay — you generally do **not** need to
manually re-issue work.

### Force a clean slate

Stop the processes, then remove the workspace's `origin-<hex>.db`,
`origin-cas-<hex>/`, the `resume/` dir (with `.mac-key`), and the trace ring.
Next launch starts fresh.

---

_Last reviewed against workspace version 0.9.8._
