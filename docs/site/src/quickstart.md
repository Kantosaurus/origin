# Quickstart

This page gets you from a clean machine to a streaming `origin` turn in under
five minutes. `origin` ships as a daemon (`origin-daemon`) and a thin client
(`origin-cli`); the client speaks `origin-ipc` over a Unix socket on
macOS/Linux and a named pipe on Windows.

## Install

The fastest path is `cargo binstall`, which fetches a signed binary built by
our release pipeline rather than compiling from source:

```bash
cargo binstall origin-cli
```

Other channels:

```bash
# Homebrew (macOS / Linux)
brew install Kantosaurus/tap/origin

# winget (Windows)
winget install Kantosaurus.origin

# AUR (Arch)
yay -S origin-bin
```

A from-source build also works, but expects Rust 1.83 (the workspace MSRV):

```bash
git clone https://github.com/Kantosaurus/origin
cd origin
cargo install --path crates/origin-cli --locked
```

The `--locked` flag matters: `origin`'s transitive deps are pinned through
`cargo update --precise` to keep edition-2024 out of the tree.

## Start the daemon

`origin-cli` auto-spawns the daemon the first time you run `origin` — and by
default it routes through `origin-supervisor`, which owns `origin-daemon`,
restarts it on panic, and applies self-dev binary hot-swaps (the exit-86
relaunch sentinel). There is nothing to start by hand:

```bash
origin                       # launches the TUI; the supervised daemon comes up on demand
```

Set `ORIGIN_NO_SUPERVISOR=1` to spawn `origin-daemon` directly instead — no
crash-restart and no self-dev hot-reload, useful when debugging a daemon panic.
Across a supervised restart, in-flight sessions resume from SQLite + the WAL
checkpoint (see [Architecture](architecture.md) for the runtime split).

## Run your first prompt

One-shot, headless:

```bash
origin run --prompt "Summarize this repo's top-level crates"
```

This opens a new session, streams the response to stdout, and exits when the
agent loop reports no further `tool_use` blocks. Add `--json` for a structured
event stream suitable for piping into other tools.

To stay interactive, just launch the TUI:

```bash
origin
```

The TUI uses `origin-tui`'s cell-grid renderer with SIMD damage diffing. Press
`?` to bring up the metrics side panel (`?metrics`), `Tab` to focus the
permission/proposal panel, and `Ctrl-C` twice to quit.

## Take the tour

A built-in interactive tutorial walks through prompting, permissions, skills,
hooks, and the side panel without touching real provider credentials:

```bash
origin --tutorial
```

The tutorial runs against a recorded provider bundle via `origin-replay`, so
it is deterministic and offline.

## Next steps

- [Configuration](configuration.md) — `~/.origin/config.toml`, env vars, the
  `origin config` subcommand.
- [Providers](providers.md) — wiring up Anthropic, OpenAI, Gemini, Bedrock,
  OpenRouter, Ollama, and GitHub Models.
- [Migration](migration.md) — importing your existing Claude Code, jcode, or
  opencode sessions and skills.
