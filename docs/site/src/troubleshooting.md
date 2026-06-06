# Troubleshooting

When something goes wrong, `origin`'s diagnostic surface is built around two
things: the structured trace parquet ring (`origin trace query`) and the
in-TUI `?metrics` panel. Everything else is a special case of one of these.

## Common errors and what they mean

### Provider 429 / rate-limit

The retry executor honors `Retry-After` and applies an exponential backoff
respecting per-error-kind policy declared in `impl Retryable for
ProviderError`. You'll see a `RateLimited` event in the side panel with the
delay. If retries exhaust the per-error policy budget you'll get a
`ProviderUnavailable` error that the model can recover from — typically by
falling back to a configured secondary provider.

To see the recent rate-limit history:

```bash
origin trace query --span provider.request --status throttled --since 1h
```

### Sandbox denial (`EPERM` from a tool)

`origin-tools` runs tools inside per-platform sandbox profiles (landlock +
seccomp + namespaces on Linux, `sandbox-exec` on macOS, AppContainer on
Windows). If a tool reports `EPERM` on a path that "should" be readable,
the profile is the first suspect:

```bash
# What profile did this Bash invocation run under?
origin trace query --span tool.exec --tool Bash --since 10m

# Loosen the tier (interactive prompt; logged)
origin permissions override-tier Bash --tier RequiresPermission/Medium
```

Per-skill `allowed-tools` narrowing also drives sandbox profile selection,
so a skill that omits a tool will cause that tool's calls to deny rather
than to prompt. See [Skills](skills.md).

### KeyVault: "credential not found"

Symptom: `origin run` reports `KeyVaultMissing { provider, account }`.
Cause: the provider is enabled in `config.toml` but no credential is stored
under that account name. Fix:

```bash
origin keyring list
origin keyring set anthropic:personal   # interactive paste
origin keyring login github-models      # OAuth device flow

# Audit shows every keychain access (separate from regular tracing)
origin keyring audit --since 24h
```

On Linux without a session bus, KeyVault falls back to an age-encrypted file
— if that file is missing or has the wrong passphrase, you'll see the same
error. `origin keyring doctor` diagnoses the fallback path.

### MCP server crashes / quarantine

An MCP server that misses `heartbeat_misses_to_quarantine` consecutive
heartbeats is quarantined and its tools fail fast with a structured error.
Check status:

```bash
origin mcp status
# github-mcp        Quarantined   last_heartbeat: 47s ago  reason: ECONNRESET
# internal-search   Healthy       last_heartbeat: 2s ago

# Trace for the underlying transport error
origin trace query --span mcp.transport --server github-mcp --since 10m

# Manual reconnect (auto-reconnect also runs on a backoff)
origin mcp reconnect github-mcp
```

See [MCP](mcp.md) for the heartbeat/quarantine configuration.

## Where logs go

`origin` does not write a single line-oriented log file. Every IPC request,
agent turn, tool call, provider call, sidecar job, and hook dispatch is a
typed `tracing` span written to a per-day compressed parquet ring at
`~/.origin/trace/` (64MB rotation, default 14-day retention). This means
queries are fast and bounded — no `grep | awk` over multi-gigabyte text.

Query with the `origin trace query` subcommand:

```bash
# All errors in the last hour
origin trace query --status error --since 1h

# A specific session's agent loop spans
origin trace query --session <id> --span agent.turn

# Drill into a tool execution
origin trace query --span tool.exec --tool Edit --since 30m --json | jq

# Live tail
origin trace query --follow
```

Add `--export` to write the result as a portable `.origin-replay` bundle a
maintainer can load deterministically.

## The `?metrics` panel

Inside the TUI, press `?` and type `metrics` (or just `?metrics`) to toggle
the metrics side panel. It surfaces:

- Live token accounting: `in / out / cache_read / cache_write / cost($) / time`
  for the current turn, updated as the stream progresses.
- Per-class task counts (Critical / Realtime / Sidecar / Background / Bulk).
- Cache hit rate per CachePlanner band (Frozen / Sticky / Sliding / Volatile).
- Arena RSS per subsystem (agent_*, cas, sidecar, swarm_*, ipc).
- MCP server health and quarantine state.

For aggregate rollups across sessions:

```bash
origin usage --since 7d
```

## Daemon won't start

```bash
# Status from the supervisor
origin daemon status

# Recent supervisor restarts (panic-then-resume)
origin trace query --span supervisor.restart --since 24h

# Tail the daemon's structured spans
origin daemon logs --follow

# Launch without the supervisor so a daemon panic is terminal (logged to
# ~/.origin/daemon.log) instead of being auto-restarted
ORIGIN_NO_SUPERVISOR=1 origin
```

If SQLite corruption is suspected, `origin admin vacuum-db --check` runs a
read-only integrity check, and `origin admin gc-cas --report` validates CAS
shard hashes without making any changes.
