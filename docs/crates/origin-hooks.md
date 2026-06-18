# origin-hooks

> Pre-spawned shell pool and typed lifecycle event dispatch.

## Purpose

`origin-hooks` lets users wire shell scripts into the agent's lifecycle without
paying a `fork+exec` per event. Hook programs are **long-lived NUL-framed
responders** held in a pre-spawned [`ShellPool`]; the daemon serializes a typed
[`LifecycleEvent`] to one line of stdin and reads the script's JSON verdict back
up to a NUL terminator. A missing or empty config means no hooks and a
byte-identical agent path. Drop-in compatibility with Claude `hooks.json` event
names is provided through aliases.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `ShellPool` | struct | Pool of long-lived workers; `new(spec, size)`, `dispatch`, `spawn_count`. |
| `ShellSpec` | struct | `{ program, args, read_terminator }` for one worker. |
| `PoolError` | enum | `Spawn` / `StdinClosed` / `StdoutClosed` / `FramingViolation`. |
| `LifecycleEvent` | enum | Typed lifecycle events (tagged JSON by `kind`). |
| `ToolPhase` | enum | `Ok` / `Err` / `Skipped`. |
| `HookOverride` / `HookOverrideInner` | type | Parsed hook verdict (`Allow`/`Deny`/…). |
| `parse_hook_stdout` | fn | Decode a hook's stdout bytes into a `HookOverride`. |
| `HookParseError` | enum | Hook stdout parse failures. |
| `dispatch_event` | fn | Serialize an event to a pool, parse the override back. |
| `DispatchError` | enum | `Pool` / `Ser` / `Parse`. |
| `HooksConfig`, `HookEntry`, `HookEventKind`, `ConfigError` | type | Config loading + Claude-name aliases. |

## Key types

```rust
pub struct ShellSpec {
    pub program: String,
    pub args: Vec<String>,
    pub read_terminator: u8,   // standardised on NUL
}

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LifecycleEvent {
    PrePrompt { text }, PostPrompt { text },
    PreTool { tool, args_preview, sandbox_ordinal },   // override-capable
    PostTool { tool, phase, sandbox_ordinal },
    PreCommit { branch }, PostCommit { sha },
    SessionStart, SessionEnd,
    MessageDisplay { text },                            // → origin-outputstyle
    BeforeModel { model }, AfterModel { model },
    PreCompress { current_bytes }, Notification { message },
}
```

## How it works

```
spawn pool once:  ShellPool::new(spec, size)  → N piped Children
per event:        dispatch_event(pool, &event)
                    │ serde_json line + '\n'  ─stdin─►  hook script
                    │ read_until(NUL)         ◄─stdout─  one JSON object + NUL
                    └► parse_hook_stdout → HookOverride
```

The amortized cost per dispatch is **one `write_all` + one `read_until`**, not a
fresh process spawn (N9.7). Workers are addressed round-robin (`next.fetch_add`);
if the chosen worker has died, a fresh one is spawned into its slot and the
dispatch is retried once, with `spawn_count` tracking every spawn (including
respawns) so tests can assert no per-event spawn. `PreTool` is the
override-capable event: a `Deny` verdict skips the tool; it carries the
`sandbox_ordinal` the daemon will enforce so a hook can short-circuit without
round-tripping the permission engine. `HooksConfig::load` reads
`~/.origin/hooks.json` (events → programs); `HookEventKind::from_label` accepts
both the canonical `snake_case` tags and Claude names (`PreToolUse`, `Stop`,
`PreCompact`, …) for drop-in compatibility.

## Dependencies & features

`tokio` (process / io-util / sync / time), `serde` + `serde_json`, `thiserror`,
and workspace `origin-sandbox` (for `ProfileOrdinal` on the tool events). A
`shellpool` Criterion bench (`harness = false`) guards the amortized-dispatch
cost. No Cargo features.

## Used by

`Grep "origin-hooks" glob "crates/*/Cargo.toml"`:

- `crates/origin-cli/Cargo.toml`
- `crates/origin-daemon/Cargo.toml`
- `crates/origin-hooks/Cargo.toml` (self)

## Testing

`tempfile` + `tokio` test-util tests drive real hook scripts through the pool:
round-trip event → override decoding, the respawn-on-dead-worker retry path
(asserted via `spawn_count`), framing-violation detection, passthrough on empty
stdout, and `HookEventKind::from_label` accepting Claude aliases. The `shellpool`
bench measures per-dispatch cost to keep it at one write + one read.

## See also

- [skills subsystem](../subsystems/skills.md)
- [tui-and-cli subsystem](../subsystems/tui-and-cli.md)
- [runtime-and-concurrency architecture](../architecture/runtime-and-concurrency.md)
- [crates index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
