# Deployment

Operational guide to **installing, updating, and running** `origin` in the
field. `origin` ships as a **single native binary** plus a thin set of packaging
shims; there is no server to stand up, no container to schedule, and no database
to provision. This page covers the install channels, the auto-update behaviour
(and how to turn it off), the local-daemon execution model, remote daemons over
QUIC+mTLS at a glance, and system requirements.

> Cross-links: daemon lifecycle in
> [`daemon-and-supervisor.md`](./daemon-and-supervisor.md); the two-runtime model
> in [`../architecture/runtime-and-concurrency.md`](../architecture/runtime-and-concurrency.md);
> first-run health checks in [`observability-runbook.md`](./observability-runbook.md).

---

## Install channels at a glance

| Channel | Command | Updates | Notes |
|---|---|---|---|
| **npm** (recommended) | `npm install -g @kantosaurus/origin` | Auto (on by default) | Scoped package; postinstall fetches the per-platform binary. Wrapper command is `origin`. |
| **cargo-binstall** | `cargo binstall origin-cli` | Manual (`cargo binstall` again) | Prebuilt release asset, no compile. |
| **Homebrew** | `brew install kantosaurus/tap/origin` | `brew upgrade` | Downloads the signed release binary for your arch. |
| **winget** | `winget install Kantosaurus.origin` | `winget upgrade` | Windows MSIX/installer manifest. |
| **AUR** | `yay -S origin-bin` (or `paru`) | Re-build from AUR | `provides=origin`, `conflicts=origin`. |
| **From source** | `cargo build --release -p origin-cli -p origin-daemon` | `git pull` + rebuild | MSRV-pinned toolchain (see below). |

All binary channels publish the **same release artifacts** named by target
triple, e.g. `origin-x86_64-unknown-linux-gnu`,
`origin-aarch64-apple-darwin`, `origin-x86_64-pc-windows-msvc`. The Homebrew,
AUR, and winget manifests pin a SHA-256 per artifact.

### npm channel details

The npm package `@kantosaurus/origin` is a launcher (`bin/origin.js`) plus
per-platform `optionalDependencies` (`@kantosaurus/origin-<os>-<arch>`). On
install:

```sh
npm install -g @kantosaurus/origin
origin --version
```

The launcher locates the native binary for `process.platform`/`process.arch`. If
the per-platform optional dependency was skipped (e.g.
`npm ci --ignore-scripts --omit=optional`), it fetches the binary synchronously
on first run, then re-resolves and `exec`s it as a transparent passthrough (the
TUI gets the real controlling TTY — raw mode, alternate screen, mouse, SIGWINCH).

### Build from source

```sh
git clone https://github.com/Kantosaurus/origin
cd origin
cargo build --release -p origin-cli -p origin-daemon
./target/release/origin --version
```

The workspace pins its toolchain via `rust-toolchain.toml`; CI builds the
shipping binaries on **Rust 1.96.0** and tests against MSRV **1.83**
(`origin doctor` fails below 1.83). Linux release builds link **glibc (gnu)**,
not musl — `origin-mem`'s ONNX Runtime dependency ships no musl prebuilt — so
Alpine/fully-static targets are unsupported out of the box.

---

## Auto-update behaviour

Auto-update is **ON by default** and is **best-effort and non-blocking** — it
never adds startup latency and never blocks the TUI.

There are two update paths; the npm launcher chooses one:

| Path | When | Mechanism |
|---|---|---|
| **Binary self-updater** (default) | `ORIGINX_ALLOW_SELF_UPDATE` unset/true | The native binary checks the registry, downloads + SHA-256-verifies the matching release asset (no `cosign` CLI needed), and swaps itself in place. |
| **npm channel** | `ORIGINX_ALLOW_SELF_UPDATE=0` | A detached, unref'd background worker (`update-check.js`) runs `npm install -g @kantosaurus/origin@<latest>`; takes effect next launch. |

The npm-channel worker is defensive:

- **Rate-limited**: at most one registry check per **24 h** (cached).
- **Single-flight**: an atomic-mkdir lock means only one worker per machine
  performs the check/install; a stale lock (>1 h) is reclaimed.
- **Respects your registry**: uses your `.npmrc`, so private registries work.
- **Global vs local aware**: `npm install -g …` for global installs;
  `npm install … --no-save` in the project root for local ones (exotic layouts
  are skipped rather than risk store corruption).
- **Never throws**: EACCES (root-owned prefix), EBUSY (Windows, replacing a
  running `.exe`), network errors are logged and the 24 h cache prevents a retry
  storm.

When a background update installs a newer version, a marker file makes the
launcher announce it **once** on the next launch:
`origin: updated to vX.Y.Z (now active).`

### Opting out

| Goal | Set |
|---|---|
| Disable **all** updates (both paths) | `ORIGINX_NO_UPDATE=1` (or `ORIGIN_NO_UPDATE=1`) |
| Use the npm channel instead of the binary self-updater | `ORIGINX_ALLOW_SELF_UPDATE=0` |

```sh
# Pin the version — no checks, no downloads, no surprises:
export ORIGINX_NO_UPDATE=1
```

`origin doctor` discloses this up front in its phone-home section:
`npm auto-update check (disable with ORIGINX_NO_UPDATE=1)`. The two opt-out
variable names (`ORIGINX_NO_UPDATE` / `ORIGIN_NO_UPDATE`) are both honoured;
prefer `ORIGINX_NO_UPDATE`.

> **Air-gapped / locked-down fleets:** set `ORIGINX_NO_UPDATE=1` in the
> machine/profile environment and distribute binaries through your own channel
> (binstall against an internal mirror, or your package manager). The only other
> outbound traffic is provider API calls to endpoints you configure and opt-in
> telemetry (off by default).

---

## Where it runs: the single-binary, local-daemon model

`origin` is one binary that wears two hats:

```
┌─────────────────────────────────────────────────────────┐
│ your terminal                                            │
│   origin (CLI/TUI)  ──spawns/supervises──▶  origin-daemon │
│        │                                        │         │
│        └──────── local IPC (socket/pipe) ───────┘         │
└─────────────────────────────────────────────────────────┘
```

- The **CLI/TUI** is what you launch. It locates (or spawns) a **daemon** scoped
  to the current workspace and talks to it over a **local IPC endpoint**.
- The endpoint is **per-workspace**: a stable hash of the canonicalized
  workspace root yields a distinct named pipe / socket, so `origin` in *n*
  projects gives *n* independent daemons that never interfere. Launching twice
  from the same directory reuses that directory's daemon.

| Platform | Default IPC endpoint |
|---|---|
| Windows | `\\.\pipe\origin-<instance-hex>` |
| Unix/macOS | `$TMPDIR/origin-<instance-hex>.sock` |

`ORIGIN_SOCK=<path>` overrides the endpoint entirely (shared/global daemon,
tunnels, tests), bypassing per-workspace scoping. The daemon is normally kept
alive by a small **supervisor** that restarts it on crash and resumes sessions —
see [`daemon-and-supervisor.md`](./daemon-and-supervisor.md).

### Provider credentials

The agent needs a provider key to do useful work. The legacy convention is the
environment variable:

```sh
export ANTHROPIC_API_KEY=sk-ant-...
origin
```

On startup the daemon mirrors `ANTHROPIC_API_KEY` into its credential vault for
back-compat. Configure other providers per
[`../subsystems/providers.md`](../subsystems/providers.md). `origin doctor` hard-fails
if **no** provider is configured.

---

## Remote daemon over QUIC + mTLS (at a glance)

The local socket/pipe is the default, but `origin-ipc` also offers a
**QUIC + mutual-TLS** transport so a thin local CLI can drive a daemon on
another host (a beefier dev box, a sandboxed build VM):

- Transports live in `crates/origin-ipc/src/`: `transport` is the local
  socket/named-pipe `Connection`; `quic` is the QUIC + mTLS remote transport.
- **Mutual TLS** means both ends present certificates — the daemon authenticates
  the client and the client authenticates the daemon. `cargo-deny` enforces a
  **rustls-only** TLS policy across the dependency graph (no OpenSSL).
- Point the CLI at a remote endpoint with `ORIGIN_SOCK` (or the equivalent
  connect flag) once the QUIC listener is bound and certs are provisioned.

> This is an advanced topology; for the common single-machine case you never
> touch it. Treat the daemon's reachable surface as security-sensitive and keep
> the local endpoint user-private.

---

## System requirements

| Requirement | Minimum / note |
|---|---|
| OS | Linux (glibc), macOS 12+, Windows 10/11. Alpine/musl-static unsupported. |
| Arch | x86_64 or aarch64 (Apple Silicon native; Intel Macs run the arm64 build via Rosetta 2). |
| Node (npm channel only) | Node **≥ 18** (the launcher; the binary itself has no Node dependency). |
| Rust (source builds only) | MSRV **1.83**; release toolchain **1.96.0**. |
| Disk | Tens of MB for the binary; the daemon writes a parquet trace ring (rotates at 64 MiB) and a SQLite session DB under the platform data dir. |
| Memory | Default supervisor soft budget **1 GiB** (`ORIGIN_SUPERVISOR_MEM_BUDGET_MB`); background sessions are shed under pressure. |
| Network | Outbound to your provider endpoint(s). Update checks + opt-in telemetry only if enabled. |

### Verify an install

```sh
origin --version          # prints the workspace version
origin doctor             # toolchain, config, daemon, providers, home, network
```

`origin doctor` runs six probes and prints the phone-home disclosure; treat any
**Fail** (missing provider, unwritable home, sub-MSRV toolchain) as a blocker
before deploying. See [`observability-runbook.md`](./observability-runbook.md).

---

## Uninstall

| Channel | Command |
|---|---|
| npm | `npm uninstall -g @kantosaurus/origin` |
| Homebrew | `brew uninstall origin` |
| winget | `winget uninstall Kantosaurus.origin` |
| AUR | `yay -R origin-bin` |
| source | delete the built binary; remove the data dir if desired |

Per-workspace daemon state lives under the OS temp/data dir
(`origin-<hex>.db`, `origin-cas-<hex>/`, the trace ring, `daemon.log`) and the
control files under `<home>/.origin/daemons/`; remove these to fully reset.

---

_Last reviewed against workspace version 0.9.8._
