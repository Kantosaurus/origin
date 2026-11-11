<div align="center">

<img src="assets/logo.svg" alt="origin" width="320">

# origin

**A Rust-native agentic coding harness — a CLI plus a supervised daemon that runs LLM-driven coding sessions locally.**

[![CI](https://github.com/Kantosaurus/origin/actions/workflows/ci.yml/badge.svg)](https://github.com/Kantosaurus/origin/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/Kantosaurus/origin/branch/dev/graph/badge.svg)](https://codecov.io/gh/Kantosaurus/origin)
[![Scorecard](https://api.securityscorecards.dev/projects/github.com/Kantosaurus/origin/badge)](https://scorecard.dev/viewer/?uri=github.com/Kantosaurus/origin)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
![MSRV](https://img.shields.io/badge/rustc-1.83+-blue.svg)
![unsafe: forbidden](https://img.shields.io/badge/unsafe-forbidden-success.svg)

</div>

`origin` runs on the same shape of *model + tools + skills + hooks + permissions*
as Claude Code, jcode, and opencode, with performance checked in CI: the perf-gate
workflow fails if a read-only task takes over 80 ms; cold start, keystroke-to-pixel
latency, steady RSS and cache hit rate are measured but not yet asserted. It draws *attributes* from those
harnesses; every signature subsystem uses an original mechanism.

It's a Cargo workspace of 40+ crates with a clean split: **`origin-daemon`** hosts
sessions, **`origin-cli`** is a thin client, and they talk only through
**`origin-ipc`**.

> **Status:** pre-1.0 (`0.9.14`). The core — content-addressed storage, archived
> IR, the two-runtime daemon, planner, provider catalog, KeyVault, and
> deterministic replay — is landed and gated. Some subsystems are "real but
> young." **No OS sandbox is active on any shipped build** (see
> [Security model](docs/security/security-model.md#sandboxing-per-platform-origin-sandbox)).

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
- **Skills as a one-line catalog.** Every installed skill is listed in the
  system prompt by name and description only; a skill's full body is injected
  after it is activated (`/<name>`), and its `allowed-tools` list then narrows
  the tool set. A skill that omits `Bash` *can't* shell out.
- **Secrets isolated in `KeyVault`.** OS-native credential stores with an
  age-encrypted fallback; `Secret<T>` redacts in `Debug`. A lint
  (`cargo run -p xtask -- lint-secrets`) flags `Debug` structs whose
  secret-named fields are not `Secret<…>` or `#[redact]`; it is not run in CI
  yet and reports existing violations. Child processes get an allow-listed
  environment without provider keys, and known credential values are redacted
  from tool output.
- **Permission modes, not a sandbox.** The default mode `workspace` lets edits
  inside the workspace run and asks before Bash, network tools and writes
  outside it; headless runs deny what would ask. The `origin-sandbox` crate has
  per-OS backends, but none is enabled on shipped builds, so nothing a tool
  spawns is OS-confined.
- **Remote IPC via QUIC + mTLS**, **deterministic replay** (`.origin-replay`
  bundles), and **zero `unsafe` in surface crates** (forbidden workspace-wide,
  audited exceptions in `cas`/`tui`/`ipc`).

## Install

The fastest way to get the `origin` command on your `PATH` — no Rust toolchain
required:

```sh
npm install -g @kantosaurus/origin   # ships a prebuilt binary; the command is `origin`
origin                               # launches the TUI
```

> The npm package is **`@kantosaurus/origin`** (scoped — the unscoped name was
> unavailable); the installed command is always **`origin`**. npm pulls a single
> small prebuilt binary for your platform, with a GitHub-release download
> fallback. It **auto-updates by default** (background npm check once/day, for
> both global and project-local installs; disable with `ORIGINX_NO_UPDATE=1`).
> See [`packaging/npm/`](packaging/npm/README.md) for details.

Other channels:

```sh
cargo binstall --git https://github.com/Kantosaurus/origin origin-cli  # prebuilt release binary (not on crates.io; no checksum verify)
cargo install --path crates/origin-cli   # build + install from source
brew install kantosaurus/origin/origin   # Homebrew tap, see packaging/homebrew
scoop bucket add origin https://github.com/Kantosaurus/scoop-origin; scoop install origin  # Windows
nix profile install github:Kantosaurus/origin   # Nix
```

For a from-source developer build, see the [Quickstart](#quickstart) below.

## Quickstart

**Prerequisites**

- Rust **1.83** or newer (the MSRV); [`rust-toolchain.toml`](rust-toolchain.toml)
  pins 1.96.0 for development and is selected automatically.
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
[Getting started](docs/guides/getting-started.md) and
[Configuration](docs/guides/configuration.md) guides.

## Documentation

The handbook is plain Markdown under [`docs/`](docs/README.md) (the mdBook site
and its GitHub Pages publish were retired). It covers the
[architecture](docs/architecture/overview.md), [subsystems](docs/subsystems/README.md),
[security model](docs/security/security-model.md),
[environment variables](docs/reference/environment-variables.md), and
[troubleshooting](docs/operations/troubleshooting.md).

## Repository layout

```
crates/            40+ workspace crates
  origin-daemon/   session host: agent loop, goal driver, provider wiring
  origin-cli/      thin client + TUI
  origin-ipc/      the only daemon <-> CLI channel (rkyv frames)
  origin-cas/      content-addressed storage (Hot/Warm/Cold tiers)
  origin-codegraph/ code graph + retrieval
  origin-provider* / origin-mem / origin-skills / origin-tools / origin-trace ...
docs/              Markdown handbook, design docs, security reviews
packaging/         npm (@kantosaurus/origin) / Homebrew / winget / AUR / binstall
xtask/             release stamping, manpages, repo automation
```

## Contributing

Contributions are welcome. Please read **[CONTRIBUTING.md](CONTRIBUTING.md)** for the
dev setup and the quality gates (`cargo fmt`, `cargo clippy -D warnings` with
`pedantic`/`nursery`, `cargo test`, MSRV 1.83, no new `unsafe`), and our
**[Code of Conduct](CODE_OF_CONDUCT.md)**. The project follows a
brainstorm → plan → TDD → verify workflow that is baked into the daemon's default
system prompt for interactive sessions.

## Security

Please **do not** open public issues for vulnerabilities. See
**[SECURITY.md](SECURITY.md)** for private reporting via GitHub Security Advisories.

## License

Licensed under the **[Apache License, Version 2.0](LICENSE)**. Unless you state
otherwise, any contribution you intentionally submit for inclusion in the work
shall be licensed as above, without additional terms or conditions
(per Apache-2.0 §5). See [`NOTICE`](NOTICE) for attribution.
