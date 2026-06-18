# Troubleshooting

A practical **symptom → cause → fix** reference for operating `origin`. Start
with `origin doctor`, then `tail` the daemon log; most issues localize quickly.

```sh
origin doctor                 # six probes + phone-home disclosure
ORIGIN_LOG=debug origin       # verbose run
tail -f <data>/origin/logs/daemon.log
```

> Data dir by OS: Linux `~/.local/share/origin`, macOS
> `~/Library/Application Support/origin`, Windows `%LOCALAPPDATA%\origin`.
> Cross-links: [`daemon-and-supervisor.md`](./daemon-and-supervisor.md),
> [`observability-runbook.md`](./observability-runbook.md),
> [`deployment.md`](./deployment.md).

---

## Daemon won't start

| Symptom | Likely cause | Fix |
|---|---|---|
| TUI hangs or exits immediately; doctor `daemon` not reachable | Daemon panics on boot (bad config, unwritable home, sub-MSRV) | Run `origin doctor`; fix any **Fail** (provider/home/toolchain). Read `daemon.log` for the panic. |
| Supervisor logs `restart storm` and bails | Deterministic startup failure restarting > `--max-restarts-per-min` (default 12) in 60 s | Don't raise the cap — fix the root cause first; check `daemon.log`, `origin doctor`. |
| `origin-daemon not found` from the supervisor | Binary not on `PATH` | Pass `--daemon-path </abs/path>` or put `origin-daemon` on `PATH`. |
| Two terminals fight over one daemon | A shared `ORIGIN_SOCK` is set | Unset `ORIGIN_SOCK` to get per-workspace daemons, or intentionally share one. |
| Stale endpoint / connect refused after a crash | Leftover socket/pipe from a hard kill | On Unix the stale socket is reclaimed on rebind; otherwise stop the PIDs in `<home>/.origin/daemons/<hex>.pid` and relaunch. |

```sh
# Inspect this workspace's control files:
cat "$HOME/.origin/daemons/"*.pid
```

---

## Provider auth failures

| Symptom | Likely cause | Fix |
|---|---|---|
| doctor `providers` = **Fail: none configured** | No provider key/credential | `export ANTHROPIC_API_KEY=sk-ant-...` (mirrored into the vault on boot) or configure another provider. |
| 401 / 403 from the provider in `daemon.log` | Expired / wrong key | Re-issue the key; restart so the daemon re-reads it into the vault. |
| `error_kind="rate_limit"` spikes in traces | Provider throttling | Back off / reduce concurrency; check `origin_tool_call_total{result="err"}`. |
| Works in shell, fails in CI bot | Secret not set in repo/CI | Add `ANTHROPIC_API_KEY` to GitHub secrets / GitLab CI vars (see [`ci-automation.md`](./ci-automation.md)). |

```sh
# Confirm the key is visible to the process:
echo "${ANTHROPIC_API_KEY:+set}"   # prints "set" if non-empty
```

---

## Permission prompts & sandbox denials

| Symptom | Likely cause | Fix |
|---|---|---|
| Repeated approval prompts for a tool | The action falls outside the auto-approved policy | Approve, or adjust the tool/permission policy (see [`../subsystems/tools.md`](../subsystems/tools.md)). |
| Edit/Write rejected with a path error | Target is outside the workspace / a worktree boundary | Operate inside the workspace root; relocate the file or widen the sandbox profile deliberately. |
| `origin_sandbox_violation_total` increments by `kind` | Kernel-enforced sandbox denied a file/exec | Read the `kind`/`profile` labels; if legitimate, relax the profile; if not, the sandbox did its job. |
| Resume token rejected: **MAC mismatch** / missing `.mac-key` | Tampered/edited resume file, or `.mac-key` deleted | Don't hand-edit resume files. Delete the offending `resume/<id>.json` (and re-create the session) — the load deliberately refuses to trust it. |

Sandbox denials are a **feature**: the policy core is pure and the kernel
enforces the verdict. Investigate the `kind` before loosening anything.

---

## Update problems

| Symptom | Likely cause | Fix |
|---|---|---|
| Never updates | Opt-out set | `ORIGINX_NO_UPDATE` / `ORIGIN_NO_UPDATE` is set — unset to re-enable. |
| Updates unexpectedly | Auto-update on by default | `export ORIGINX_NO_UPDATE=1` to pin the version. |
| npm update fails `EACCES` | Root-owned global prefix | Fix npm prefix ownership, or update manually: `sudo npm i -g @kantosaurus/origin@latest`. |
| npm update fails `EBUSY` (Windows) | Replacing a running `.exe` | Close all `origin` instances, then update; the 24 h cache prevents a retry storm meanwhile. |
| "updated to vX" announced but old behaviour | Update applies on **next** launch | Relaunch `origin`. |
| Self-updater errors about `cosign` | Wrong update path expectation | The binary self-updater SHA-256-verifies (no `cosign`); to force the npm channel set `ORIGINX_ALLOW_SELF_UPDATE=0`. |
| Background updater silent | Rate-limited (24 h) or lock held | Check `<cache>/…/log`; one machine-wide single-flight lock serializes updates. |

---

## Windows-specific notes

| Symptom | Likely cause | Fix |
|---|---|---|
| Link error / missing PDB during source build | MSVC toolchain / linker setup | Install the MSVC build tools (or use the prebuilt binary via npm/winget). PDBs are build artifacts, not shipped. |
| `\\.\pipe\origin-<hex>` connect refused | Daemon not bound yet, or different workspace | Wait for bind (supervisor retries ~10 s for resume); confirm same workspace (the pipe name hashes the cwd). |
| Resume key perms can't be tightened | std has no portable `chmod` on Windows | The state dir under `%LOCALAPPDATA%` must already be user-private (it is by default); keep it so. |
| Intel Mac vs arm64 confusion | Only arm64 macOS prebuilt ships | Intel Macs run the arm64 binary via Rosetta 2 — install Rosetta if prompted. |
| Ctrl-C kills the wrapper not the TUI | Signal routing | The npm launcher attaches no-op signal handlers so the child TUI owns Ctrl-C; if you bypass it, send signals to the child PID. |

---

## High RSS

| Symptom | Likely cause | Fix |
|---|---|---|
| Daemon memory grows with many sessions | Background/detached sessions accumulating | The supervisor sheds **background, oldest-idle-first** when pressure ≥ shed ratio (default 0.9 of budget); the foreground session is never shed. |
| Memory pressure but nothing shed | Budget too high, or only an interactive session | Lower `ORIGIN_SUPERVISOR_MEM_BUDGET_MB`; interactive sessions are protected by design. |
| Idle sessions linger | Grace windows | They retire after the idle grace (5 min interactive / 30 min detached); tune `ORIGIN_SUPERVISOR_IDLE_GRACE_MS` / `…_DETACHED_GRACE_MS`. |
| Trace dir growing | Parquet ring | Files rotate at 64 MiB; prune old `trace-*.parquet` if disk-constrained (postmortem-only). |
| Slow leak of tool/error label memory | Many distinct interned strings | Bounded by design: the trace interner caps at 4096 distinct strings; metric label cardinality is allowlist-bounded. |

```sh
# Tune the supervisor's soft budget and shedding for a smaller box:
export ORIGIN_SUPERVISOR_MEM_BUDGET_MB=512
export ORIGIN_SUPERVISOR_MEM_SHED_RATIO=0.85
```

---

## Cache misses / high token cost

| Symptom | Likely cause | Fix |
|---|---|---|
| `origin_cache_hit_total` rate low | Prompt cache went cold | The TUI nudges "cache just went cold"; cache TTL is ~5 min — long idle gaps re-pay the cache-write premium. |
| Token cost higher than expected | Cache cold turns / large context | Check `origin usage` / `insights` (per-model breakdown sorted by cost); keep turns flowing within the cache TTL. |
| Cache hit rate below the 70% KPI | Workload or planning regression | Inspect traces; this KPI is surfaced, not gated — investigate the token planner path. |
| `?metrics` panel shows zeros | Daemon turn-completion not wired / fresh daemon | Run a turn; the panel reads the live registry the daemon increments. |

---

## IPC / connectivity

| Symptom | Likely cause | Fix |
|---|---|---|
| CLI can't reach the daemon | Endpoint mismatch (CLI and daemon resolved different paths) | Both derive the endpoint from the canonicalized cwd; run them from the same workspace, or pin both with the same `ORIGIN_SOCK`. |
| Remote daemon connect fails | QUIC listener not up, or cert mismatch | The remote transport is **QUIC + mTLS** (both ends present certs); verify the listener is bound and the client cert is trusted. TLS is rustls-only (no OpenSSL). |
| Resume replay never re-attaches | Daemon endpoint not reachable within the retry budget | The supervisor retries ~40 × 250 ms ≈ 10 s; if the daemon takes longer to bind, increase headroom or check why boot is slow. |
| `ORIGIN_SOCK` set but ignored by a tool | Tool resolves its own per-workspace path | Ensure `ORIGIN_SOCK` is exported in the environment all the processes inherit. |

The per-workspace endpoint is `\\.\pipe\origin-<hex>` (Windows) or
`$TMPDIR/origin-<hex>.sock` (Unix), where `<hex>` is a stable 16-char hash of the
canonicalized workspace root.

---

## Self-dev binary swap / rollback

| Symptom | Likely cause | Fix |
|---|---|---|
| New self-built binary keeps reverting | It crash-looped within the rollback window | The supervisor rolls back to the backup after **≥ 2 crashes within 5 s** of a swap. Fix the new binary; the backup (`<name>.bak`) is the last-known-good. |
| Swap "failed; restarting current binary" in logs | New binary missing/empty/unreadable | Re-build; a truncated artifact fails validation and is never swapped in (the old one is untouched). |
| Daemon exits with code 86 unexpectedly | `SELFDEV_RELAUNCH_EXIT_CODE` (a relaunch request, not a crash) | Expected during self-dev; the supervisor consults `relaunch.json` and swaps. Not an error. |

---

## Diagnostics by data dir

```sh
# Linux
ls -la ~/.local/share/origin/{logs,trace}
# macOS
ls -la "$HOME/Library/Application Support/origin/"{logs,trace}
# Windows (PowerShell)
Get-ChildItem $env:LOCALAPPDATA\origin\logs, $env:LOCALAPPDATA\origin\trace
```

| File | Use it for |
|---|---|
| `logs/daemon.log` | What the daemon did right now (truncates each start). |
| `trace/trace-*.parquet` | Postmortem span analysis (query with DuckDB). |
| `selfdev/relaunch.json` | Pending self-dev binary swap handoff. |
| `<home>/.origin/daemons/<hex>.{pid,stamp}` | Which PIDs to stop; last spawn time. |

---

## Logging & trace gaps

| Symptom | Likely cause | Fix |
|---|---|---|
| `daemon.log` is empty / sparse | Verbosity too low | Start with `ORIGIN_LOG=debug` (then `RUST_LOG`); default is `info`. |
| Expected trace rows missing | Rows dropped under backpressure | The trace layer prefers dropping a row over blocking the agent loop; not an error — reproduce under lighter load if you need the row. |
| `query::run` returns nothing on a fresh daemon | No traces yet | A missing trace dir is treated as "no traces" (empty result), not a failure. |
| Logs reset after every restart | Text log truncates on each daemon start | Expected; use the parquet ring for cross-restart history. |

---

## General diagnostics flow

1. **`origin doctor`** — fix any **Fail** (toolchain/providers/home) and note
   **Warn** (config/daemon/network).
2. **`daemon.log`** (with `ORIGIN_LOG=debug`) — read the most recent error/panic.
3. **Trace ring** — for slow/intermittent issues, query parquet by `kind` /
   `error_kind` (see the observability runbook).
4. **Metrics** — bind `/metrics` and watch `origin_tool_call_total{result="err"}`,
   `origin_sandbox_violation_total`, `origin_cache_hit_total`.
5. **Clean slate** — stop the workspace PIDs, remove `origin-<hex>.db`,
   `origin-cas-<hex>/`, `resume/` (+`.mac-key`), and the trace ring; relaunch.

```sh
# Emergency reset for one workspace (stop processes first):
rm -f  "$TMPDIR"/origin-*.db
rm -rf "$TMPDIR"/origin-cas-*
# remove the resume dir + key and the trace ring under the data dir as needed
```

---

_Last reviewed against workspace version 0.9.8._
