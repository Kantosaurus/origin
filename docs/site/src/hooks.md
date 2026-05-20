# Hooks

Hooks are shell scripts (or any executable) that `origin-hooks` dispatches at
declared lifecycle points. They are how third-party tools observe and
selectively modify what the agent does — without linking against
`origin-core`.

## Lifecycle events

`origin-hooks` emits typed payloads at each of the following points. Every
event has a declared output schema; mismatches log and skip silently rather
than crashing the agent loop.

| Event | When | Override channel |
|---|---|---|
| `pre_prompt` | Before user prompt becomes a request | `modify` prompt |
| `post_prompt` | After agent loop finishes a turn | observation only |
| `pre_tool` | Before a tool executes | `block` or `modify` args |
| `post_tool` | After a tool's result lands in CAS | observation only |
| `permission_ask` | When the permission engine raises a prompt | `allow` / `deny` |
| `session_start` | New session opened | observation |
| `session_end` | Session closed (clean or crash) | observation |
| `worker_spawn` | Swarm coordinator spawns a worker | `block` |
| `worker_complete` | Worker emits its `CompletionReport` | observation |
| `memory_proposed` | Sidecar drafts a memory for review | `accept`/`reject`/`edit` |
| `skill_injected` | A skill materializes into a turn | observation |
| `compaction_triggered` | Compaction about to run | `defer` |

Blocking events (`pre_tool`, `permission_ask`, `worker_spawn`,
`memory_proposed`, `compaction_triggered`) have a default 1-second timeout;
the agent loop proceeds on timeout so a broken hook can't wedge a session.

## Pre-spawned shell pool

A naive `spawn(/bin/sh, ["-c", script])` per event costs 5–200ms — enough to
be visible on every keystroke when `pre_prompt` is hooked. `origin-hooks`
instead keeps a pool of pre-warmed shells (default `pool_size = 2`,
configurable in `[hooks]`). Each event is dispatched over stdin with a
preamble that echoes a delimiter on completion; dispatch latency drops to
~200µs.

Hooks run in the `Sidecar` task class, so they never compete with `Critical`
agent-loop work for CPU. Hook scripts also inherit the triggering tool's
sandbox profile — there is no privilege escalation path from a `pre_tool`
hook on `Read` into running `Bash`.

## Example: pre-commit lint via `pre_tool`

This hook blocks any `Edit` that would touch a file under `vendored/`:

```bash
#!/usr/bin/env bash
# ~/.origin/hooks/block-vendored-edits.sh
# Bound to: pre_tool

set -euo pipefail

# Origin pipes the event JSON to stdin.
payload="$(cat)"

tool="$(jq -r '.tool' <<<"$payload")"
path="$(jq -r '.args.file_path // empty' <<<"$payload")"

if [[ "$tool" == "Edit" && "$path" == vendored/* ]]; then
  jq -n --arg msg "Refusing to edit vendored code; ask a human." \
    '{override: "block", message: $msg}'
  exit 0
fi

# Default: no override (the agent proceeds unmodified).
echo '{}'
```

Register it in `config.toml`:

```toml
[[hooks]]
event = "pre_tool"
script = "~/.origin/hooks/block-vendored-edits.sh"
```

The harness preamble takes care of plumbing — your script reads JSON from
stdin and writes JSON to stdout. Anything written to stderr lands in the
daemon's `tracing` ring and is queryable via `origin trace query` (see
[Troubleshooting](troubleshooting.md)).

## Tips

- Keep hooks idempotent. They can be retried on transient daemon failures.
- Hooks that need to call back into `origin` itself (querying the code graph,
  searching memories) should use `origin-ipc` rather than shelling out to
  `origin-cli`, which would round-trip through a fresh client process. See
  [SDK](sdk.md) for the IPC client.
- The `permission_ask` override is how you make a CI machine fully headless:
  bind it to a script that auto-`deny`s everything not in an allowlist.
