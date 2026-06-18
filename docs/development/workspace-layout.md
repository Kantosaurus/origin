# Workspace layout

`origin` is a single Cargo workspace (`resolver = "2"`) declared in the root
`Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = ["crates/*", "xtask"]
exclude = ["crates/origin-daemon/fuzz"]
```

That is **~77 crates** under `crates/`, plus the `xtask` developer-tools binary.
The daemon's `fuzz` crate is deliberately excluded (it builds only under nightly —
see [building-and-testing.md](building-and-testing.md)). For the system-level
picture — the daemon/CLI split, the two-runtime model, the archived IR, and the
request lifecycle — read [../architecture/overview.md](../architecture/overview.md);
this page is the *physical* map.

---

## Top-level tree

| Path | Purpose |
| --- | --- |
| `crates/` | The ~77 workspace member crates (the entire product). |
| `xtask/` | Developer-tools binary: `lint-secrets`, `lint-spawn`, `manpages`, `release`. |
| `docs/` | Documentation: `architecture/`, `crates/`, `development/` (this dir), `security/`, and the published mdBook in `docs/site/`. |
| `packaging/` | Distribution: `npm/`, `homebrew/`, `winget/`, `aur/` templates + scripts. |
| `bench/` | Benchmark task fixtures (`bench/perf/tasks`) the perf gate runs against. |
| `vendor/` | Vendored third-party sidecars, e.g. `vendor/cloak-browser/` (Node CLI). |
| `.github/` | CI workflows, issue/PR templates, `CODEOWNERS`, `dependabot.yml`. |
| `Cargo.toml` | Workspace manifest: members, shared package metadata, lints, deps, profiles. |
| `Cargo.lock` | Committed; MSRV-pinned transitive deps. Builds run `--locked`. |
| `rust-toolchain.toml` | Pinned toolchain (channel `1.96.0`, `clippy` + `rustfmt`). |
| `deny.toml` | `cargo deny` config: advisories, rustls-only bans, crates.io sources. |
| `REUSE.toml`, `LICENSES/` | REUSE compliance for the Apache-2.0 SPDX headers. |
| `CONTRIBUTING.md`, `GOVERNANCE.md`, `CODE_OF_CONDUCT.md`, `ROADMAP.md`, `CHANGELOG.md`, `SECURITY.md` | Project governance and history. |

---

## The daemon / CLI / IPC split

The product is three trust-and-process boundaries plus the crates that serve them:

- **`origin-cli`** — the thin terminal client. Builds the `origin` binary, renders
  the TUI, and supervises/auto-spawns the daemon. Holds no session state.
- **`origin-daemon`** — the long-lived server process. Hosts the agent loop, goal
  driver, provider wiring, scheduler, and storage. This is where sessions live.
- **`origin-ipc`** — the *only* channel between them: a framed transport carrying
  rkyv-archived IR frames (local socket / Windows named pipe, plus a QUIC +
  mutual-TLS remote transport). No shared memory, no side channel, no direct call
  across the boundary.

If you change a frame, you are changing the wire protocol — coordinate first (see
[contributing.md](contributing.md)).

---

## The layered crate dependency story

Crates layer from pure types upward to the two binaries. Lower layers never depend
on higher ones; this is what keeps the daemon/CLI split honest and the build
parallel. (See [../architecture/overview.md](../architecture/overview.md) for the
full graph.)

### Layer 0 — foundation types

`origin-core` (the pure IR: `Role`, `Message`, `Block`, `MessageId`,
`ProviderCaps`; everything is `rkyv::Archive`), `origin-stream` (the byte ring
buffer), `origin-multimodal`.

### Layer 1 — storage & transport

`origin-cas` (content-addressed store: blake3 + FastCDC + tiered mmap/zstd —
**audited `unsafe`**), `origin-store` (SQLite + refinery migrations),
`origin-ipc` (wire frames + transports — **audited `unsafe`**), `origin-replay`,
`origin-cassette`.

### Layer 2 — runtime & policy

`origin-runtime` (`TaskClass` + `spawn_in`), `origin-permission`,
`origin-cmdparse`, `origin-conseca`, `origin-policy`, `origin-sandbox`,
`origin-keyvault` (`Secret<T>` + OS keychain), `origin-oidc`.

### Layer 3 — providers & catalog

`origin-provider` (the canonical `Provider` trait + request/response/usage/error
types) and the per-provider impls: `origin-provider-anthropic`,
`origin-provider-openai-compat`, `origin-provider-gemini`,
`origin-provider-bedrock`, `origin-provider-github`, `origin-provider-ollama`,
plus `origin-modeldiscovery`, `origin-router`, `origin-cost`.

### Layer 4 — tools, skills, memory, knowledge

`origin-tools` (compile-time tool registry + builtins), `origin-mcp`,
`origin-skills` (embedded superpowers skills), `origin-mem`, `origin-knowledge`,
`origin-codegraph`, `origin-repomap`, `origin-scout`, `origin-lsp-client`,
`origin-lspfleet`, `origin-browser`, `origin-websearch`, `origin-gmail`,
`origin-hooks`, `origin-plan`, `origin-planner`, `origin-goal`,
`origin-workflowgen`, `origin-vcs`, `origin-editfmt`, `origin-postedit`,
`origin-review`, `origin-mermaid`, `origin-export`, `origin-multimodal`,
`origin-voice`, `origin-clipboard`, `origin-notify`, `origin-watch`,
`origin-schedule`, `origin-steering`, `origin-outputstyle`, `origin-plugin`,
`origin-shimquirks`, `origin-i18n`, `origin-resume-token`, `origin-ambient`,
`origin-selfdev`, `origin-doctor`.

### Layer 5 — observability

`origin-trace`, `origin-telemetry`, `origin-metrics`, `origin-alloc`.

### Layer 6 — orchestration & UI

`origin-swarm` (coordinator/worker fan-out + admission gate), `origin-sidecar`
(small-model jobs), `origin-supervisor` (process lifecycle),
`origin-tui` (terminal grid/render — **audited `unsafe`**), `origin-ui-preview`.

### Layer 7 — the binaries

`origin-daemon` (the server; its `fuzz` subcrate is workspace-excluded) and
`origin-cli` (the `origin` binary). `origin-bench` is the internal benchmark
runner (`publish = false`), and `origin-migrate` ingests Claude Code / jcode /
opencode histories.

> The exact crate roster evolves; `ls crates/` is authoritative, and each crate
> has a one-page reference at `docs/crates/<name>.md`.

### Reading the layering

Two rules make the graph navigable:

- **Names encode the layer loosely.** `origin-core`/`origin-stream` are
  foundation; `origin-provider*` are the model layer; `origin-cli`/`origin-daemon`
  are the binaries at the top. The `origin-provider-<vendor>` suffix marks a
  concrete `Provider` impl over the `origin-provider` trait.
- **Dependencies point downward only.** If you find yourself wanting a lower-layer
  crate to depend on a higher-layer one, the abstraction is in the wrong place —
  invert it (push a trait down, or move the type to `origin-core`).

To inspect the real dependency edges for a crate:

```sh
cargo tree -p origin-daemon -e normal --depth 1   # direct deps of one crate
cargo tree -i -p origin-core                        # who depends on origin-core
```

---

## Crate metadata conventions

Every member inherits shared metadata from `[workspace.package]` and the lint
policy:

```toml
[package]
name = "origin-<thing>"
description = "…"            # required for crates.io
version.workspace = true     # 0.9.8, single source of truth
edition.workspace = true     # 2021
rust-version.workspace = true # 1.83 (MSRV)
license.workspace = true     # Apache-2.0
repository.workspace = true

[lints]
workspace = true             # inherits the pedantic/nursery/unwrap/unsafe policy
```

Internal-only crates (e.g. `origin-bench`) set `publish = false`. Shared
third-party versions are pinned once in `[workspace.dependencies]` and referenced
with `dep = { workspace = true }`. To add a member, follow
[adding-a-crate.md](adding-a-crate.md).

---

## Build profiles (Windows note)

The root manifest caps debuginfo so the all-in-one `origin-cli` binary's PDB stays
under the MSVC linker's 4 GB cap (LNK1318):

```toml
[profile.dev]
debug = "line-tables-only"
[profile.test]
debug = "line-tables-only"
```

Keep these settings when adding profiles. Details in
[building-and-testing.md](building-and-testing.md#windows-the-pdb--line-tables-only-note).

---

## Where things live (quick index)

| Looking for… | Go to |
| --- | --- |
| The IR / message types | `crates/origin-core` |
| The wire protocol | `crates/origin-ipc` |
| The agent loop | `crates/origin-daemon` |
| The TUI | `crates/origin-cli`, `crates/origin-tui` |
| Tools & their registry | `crates/origin-tools` |
| Provider trait + impls | `crates/origin-provider*` |
| Concurrency model | `crates/origin-runtime` + [../architecture/runtime-and-concurrency.md](../architecture/runtime-and-concurrency.md) |
| Storage | `crates/origin-cas`, `crates/origin-store` + [../architecture/data-and-storage.md](../architecture/data-and-storage.md) |
| Dev tooling / lints | `xtask/` |
| Benchmarks | `crates/origin-bench`, `bench/` |
| Packaging | `packaging/` |

_Last reviewed against workspace version 0.9.8._
