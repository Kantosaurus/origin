<div align="center">

# origin

**A Rust-native agentic coding harness — a CLI plus a supervised daemon that runs LLM-driven coding sessions locally.**

[![CI](https://github.com/Kantosaurus/origin/actions/workflows/ci.yml/badge.svg)](https://github.com/Kantosaurus/origin/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
![MSRV](https://img.shields.io/badge/rustc-1.83+-blue.svg)
![unsafe: forbidden](https://img.shields.io/badge/unsafe-forbidden-success.svg)

</div>

`origin` runs on the same shape of *model + tools + skills + hooks + permissions*
as Claude Code, jcode, and opencode — but it treats four performance KPIs as
**first-class CI gates** rather than afterthoughts: cold start, keystroke-to-pixel
latency, steady RSS, and cache hit rate. It draws *attributes* from those
harnesses; every signature subsystem uses an original mechanism.

It's a Cargo workspace of 30+ crates with a clean split: **`origin-daemon`** hosts
sessions, **`origin-cli`** is a thin client, and they talk only through
**`origin-ipc`**.

> **Status:** pre-1.0 (`0.0.1`). The core — content-addressed storage, archived
> IR, the two-runtime daemon, planner, provider catalog, KeyVault, sandboxing,
> and deterministic replay — is landed and gated. Some subsystems are "real but
> young"; see [`docs/why-origin.md`](docs/why-origin.md) for an honest accounting.

## Why origin

- **Perf-as-gate, not aspiration.** The [`perf-gate`](.github/workflows/perf-gate.yml)
  workflow asserts read-only tasks complete in ≤ 80 ms wall time, in CI.
- **Two-runtime daemon.** A `current_thread` control plane pinned to one OS thread;
  `multi_thread` workers. A clippy-enforced `spawn_in(class, fut)` with task classes
  keeps a tool exec or sidecar job from starving the renderer or IPC accept loop.
- **Content-addressed everything (`origin-cas`).** Tool outputs, file reads,
  embeddings, memory, and code-graph nodes are deduped across turns, sessions, and
  swarm workers, with Hot LRU / Warm mmap / Cold zstd tiers.
- **Archived IR end-to-end (`rkyv`).** One byte buffer flows through IPC, SQLite
  blobs, and ring buffers — ~200 ns to validate vs. ~20 µs to JSON-decode.
- **~40 providers behind one catalog.** Anthropic, OpenAI (API + Codex OAuth),
  Gemini, Bedrock, Ollama, GitHub Copilot, and an `openai-compat` driver for
  OpenRouter, DeepSeek, xAI, Mistral, Qwen, and more.
- **Embedding-indexed lazy skill injection.** Skills are indexed into an HNSW graph
  and materialized top-K per turn — hundreds of installed skills, zero
  session-start scan cost.
- **Secrets isolated in `KeyVault`.** OS-native credential stores with an
  age-encrypted fallback; `Secret<T>` redacts in `Debug`; a CI lint rejects raw
  secret bytes through `tracing`.
- **Sandboxing per platform.** landlock + seccomp + namespaces (Linux),
  `sandbox-exec` (macOS), AppContainer (Windows). A skill that omits `Bash` *can't*
  shell out.
- **Remote IPC via QUIC + mTLS**, **deterministic replay** (`.origin-replay`
  bundles), and **zero `unsafe` in surface crates** (forbidden workspace-wide,
  audited exceptions in `cas`/`tui`/`ipc`).

A fuller pitch and head-to-head comparison live in [`docs/why-origin.md`](docs/why-origin.md).

## Install

The fastest way to get the `origin` command on your `PATH` — no Rust toolchain
required:

```sh
npm install -g originx   # ships a prebuilt binary; the command is `origin`
origin                   # launches the TUI
```

> The npm package is named **`originx`** (the name `origin` was already taken on
> npm); the installed command is always **`origin`**. npm pulls a single small
> prebuilt binary for your platform, with a GitHub-release download fallback.
> It **auto-updates by default** (background npm check once/day, for both global
> and project-local installs; disable with `ORIGINX_NO_UPDATE=1`). See
> [`packaging/npm/`](packaging/npm/README.md) for details.

Other channels:

```sh
cargo binstall origin-cli            # prebuilt binary via cargo-binstall
cargo install --path crates/origin-cli   # build + install from source
brew install origin                  # Homebrew (tap), see packaging/homebrew
```

For a from-source developer build, see the [Quickstart](#quickstart) below.

## Quickstart

**Prerequisites**

- Rust **1.83** (the pinned toolchain is selected automatically via
  [`rust-toolchain.toml`](rust-toolchain.toml)).
- *Optional:* Node ≥ 18 for the browser sidecar; a provider API key (e.g.
  `ANTHROPIC_API_KEY`) for live sessions.

**Build & run**

```sh
git clone https://github.com/Kantosaurus/origin.git
cd origin
cargo build --release

# Launch the CLI (it supervises the daemon for you):
./target/release/origin --help
./target/release/origin            # start an interactive session
./target/release/origin --tutorial # offline, replay-driven guided tour
```

Migrating from another harness? `origin import claude-code|jcode|opencode` is
idempotent (content-hash dedupe — safe to re-run). See the full
[Quickstart](docs/site/src/quickstart.md) and
[Configuration](docs/site/src/configuration.md) guides.

## Documentation

The book lives in [`docs/site/`](docs/site/src/SUMMARY.md) (mdBook) and covers
[architecture](docs/site/src/architecture.md), [providers](docs/site/src/providers.md),
[skills](docs/site/src/skills.md), [hooks](docs/site/src/hooks.md),
[MCP](docs/site/src/mcp.md), [migration](docs/site/src/migration.md), the
[SDK](docs/site/src/sdk.md), and [troubleshooting](docs/site/src/troubleshooting.md).

## Repository layout

```
crates/            30+ workspace crates
  origin-daemon/   session host: agent loop, goal driver, provider wiring
  origin-cli/      thin client + TUI
  origin-ipc/      the only daemon <-> CLI channel (rkyv frames)
  origin-cas/      content-addressed storage (Hot/Warm/Cold tiers)
  origin-codegraph/ code graph + retrieval
  origin-provider* / origin-mem / origin-skills / origin-tools / origin-trace ...
docs/              mdBook site, design specs & plans, security reviews
packaging/         npm (originx) / Homebrew / winget / AUR / binstall
xtask/             release stamping, manpages, repo automation
```

## Contributing

Contributions are welcome. Please read **[CONTRIBUTING.md](CONTRIBUTING.md)** for the
dev setup and the quality gates (`cargo fmt`, `cargo clippy -D warnings` with
`pedantic`/`nursery`, `cargo test`, MSRV 1.83, no new `unsafe`), and our
**[Code of Conduct](CODE_OF_CONDUCT.md)**. The project follows a
brainstorm → plan → TDD → verify workflow that is baked into the daemon's default
system prompt.

## Security

Please **do not** open public issues for vulnerabilities. See
**[SECURITY.md](SECURITY.md)** for private reporting via GitHub Security Advisories.

## License

Licensed under the **[Apache License, Version 2.0](LICENSE)**. Unless you state
otherwise, any contribution you intentionally submit for inclusion in the work
shall be licensed as above, without additional terms or conditions
(per Apache-2.0 §5). See [`NOTICE`](NOTICE) for attribution.
