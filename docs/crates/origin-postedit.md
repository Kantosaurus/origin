# origin-postedit

> Post-edit lint/test/format policy with a builtin formatter table; decision logic only, execution is the caller's job

## Purpose

After the agent edits a file, the daemon must decide what to do next: which
formatter to run, whether to lint and test, and — when a check fails — how many
times to let the model attempt a repair before giving up. `origin-postedit` is
that pure decision layer. It ships an opencode-parity formatter table (~25
auto-formatters), an aider-style `auto_lint`/`auto_test` config with overrides,
and a bounded repair loop. It never spawns a process or touches the filesystem;
the caller executes the chosen commands.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `FormatterRule` | struct | `{ ext, command }` static mapping. |
| `formatter_for` | fn | Builtin formatter lookup by path extension. |
| `PostEditConfig` | struct | `auto_lint`/`lint_command`/`auto_test`/`test_command`/`format_overrides`/`max_repair_iters`. |
| `PostEditConfig::formatter_for` | fn | Overrides-first formatter resolution. |
| `PostEditConfig::validate` | fn | Reject empty overrides / lint-without-command. |
| `RepairDecision` | enum | `Stop` / `Retry { iter }` / `GiveUp`. |
| `repair_decision` | fn | Decide the next step of the repair loop. |
| `PostEditError` | enum | `EmptyOverride` / lint-command-missing. |

## Key types

```rust
pub struct PostEditConfig {
    pub auto_lint: bool,
    pub lint_command: Option<String>,
    pub auto_test: bool,
    pub test_command: Option<String>,
    pub format_overrides: Vec<(String, String)>, // (ext, command), case-insensitive
    pub max_repair_iters: u32,                    // default 2
}

pub enum RepairDecision {
    Stop,                 // no failures — clean
    Retry { iter: u32 },  // 1-based attempt index
    GiveUp,               // budget exhausted
}
```

## How it works

The builtin `FORMATTERS` table maps lowercase extensions to commands
(`rs`→`rustfmt`, `go`→`gofmt`, `py`→`ruff format`, the whole JS/TS/web family
→`prettier`, `c`/`cpp`→`clang-format`, plus Kotlin, Elixir, Ruby, shell, Lua,
TOML, Dart, Swift, Zig, Nix, Terraform, Java). `PostEditConfig::formatter_for`
checks `format_overrides` first (case-insensitive), then falls back to the
builtin table; the caller appends the target path. After running checks the
daemon calls `repair_decision(failures, prev_iters, cfg)`: zero failures →
`Stop`; failures with budget left → `Retry { iter: prev_iters + 1 }`; otherwise
`GiveUp`. The whole flow is `const`/pure and deterministic.

```
edit ─▶ formatter_for(path) ─▶ command (caller runs it)
     ─▶ run lint/test (caller) ─▶ failures count
                                     │
        repair_decision(failures, prev_iters, cfg) ─▶ Stop | Retry{iter} | GiveUp
```

## Dependencies & features

`#![forbid(unsafe_code)]`. Only `serde` (config serialization) and `thiserror`
(`PostEditError`). No process spawning, no async — std-only and deterministic.
Dev-dep `serde_json` backs round-trip tests.

## Used by

```
crates/origin-daemon/Cargo.toml
crates/origin-postedit/Cargo.toml
```

## Testing

The doctest demonstrates `formatter_for("src/main.rs") == Some("rustfmt")` and
the repair-loop transitions (`repair_decision(0,0,..) == Stop`,
`repair_decision(2,0,..) == Retry{iter:1}`). In-file tests cover the
override-wins-over-builtin path, `validate` rejections, and the
`Stop`/`Retry`/`GiveUp` boundaries against the default `max_repair_iters` of 2.

## See also

- [Tools subsystem](../subsystems/tools.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
