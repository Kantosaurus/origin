# Building and testing

This page is the command catalog for building, testing, linting, measuring
coverage, running the performance gate, and fuzzing `origin` locally. Every
command here mirrors a step that CI runs (`.github/workflows/`), so a green local
run is a strong predictor of a green PR.

---

## Prerequisites

- **Rust 1.83+** — `1.83` is the MSRV; the repo pins a toolchain in
  [`rust-toolchain.toml`](../../rust-toolchain.toml) (currently channel `1.96.0`
  with `clippy` + `rustfmt`). `rustup` installs it automatically on first build in
  the repo directory. You do not need to install a toolchain by hand.
- **A C toolchain / linker** for native crates (`rusqlite` bundles SQLite;
  `zstd`/`tar` build native code). On Windows use the MSVC build tools; on
  macOS install the Command Line Tools; on Linux a `cc` + `pkg-config` is enough.
- *Optional:* **Node ≥ 18** for the browser sidecar (`origin-browser` /
  `vendor/cloak-browser`), and a provider API key (e.g. `ANTHROPIC_API_KEY`) for
  anything that hits a live model. Live tests are skipped when no key is set.

TLS is **rustls-only** workspace-wide — there is no OpenSSL dependency to satisfy,
and `cargo deny` enforces the ban.

---

## Build

```sh
cargo build                       # whole workspace, debug
cargo build --release             # whole workspace, optimized
cargo build -p origin-cli         # just the CLI (binary: origin)
cargo build --release -p origin-cli -p origin-daemon   # the two release binaries
```

The workspace is `resolver = "2"`, `members = ["crates/*", "xtask"]`, and
**excludes** `crates/origin-daemon/fuzz` (the fuzz crate is built separately under
nightly — see [Fuzzing](#fuzzing)).

### Running the CLI from target

`origin-cli` produces the `origin` binary and supervises/auto-spawns
`origin-daemon`; the two communicate **only** over `origin-ipc`.

```sh
cargo run -p origin-cli -- --help        # via cargo
./target/release/origin --help           # the built binary directly
./target/debug/origin --version
```

### Windows: the PDB / line-tables-only note

`origin-cli` links the daemon, every provider, and every tool into one binary.
With full debuginfo the resulting PDB blows past the MSVC linker's hard 4 GB cap
(**LNK1318**). The workspace therefore limits debuginfo in `Cargo.toml`:

```toml
[profile.dev]
debug = "line-tables-only"

[profile.test]
debug = "line-tables-only"
```

This keeps backtrace line numbers but drops type info. If you add a new profile or
override `debug` locally on Windows, keep it at `line-tables-only` (or lower) for
the `origin-cli` build or expect LNK1318. Release builds are unaffected.

---

## Test

```sh
cargo test --workspace                 # unit + integration tests, whole workspace
cargo test --workspace --locked        # exactly what CI runs (no Cargo.lock drift)
```

### Running a single crate's tests

```sh
cargo test -p origin-runtime                       # one crate
cargo test -p origin-daemon --test swarm_worker_e2e # one integration test file
cargo test -p origin-cas store::                    # filter by test-name substring
cargo test -p origin-provider -- --nocaptured       # see println!/tracing output
cargo test -p origin-tui -- --test-threads=1        # serialize (useful for TUI)
```

Conventions you will see across the tree:

- Unit tests live in a `#[cfg(test)] mod tests { … }` block at the bottom of the
  module they cover.
- Integration tests live in `crates/<name>/tests/*.rs` (one binary per file).
- Property tests use `proptest` (a workspace dependency); provider/wire parsers and
  the IPC frame validator are common targets.
- Live-network tests self-skip when the relevant key is absent (e.g.
  `anthropic_smoke` prints "skipping live_smoke" and exits 0 with no
  `ANTHROPIC_API_KEY`).

---

## Lint and format

```sh
cargo fmt --all                                          # apply formatting
cargo fmt --all -- --check                               # verify (CI gate)
cargo clippy --workspace --all-targets --locked -- -D warnings   # CI gate
```

The lint policy (clippy `pedantic` + `nursery` as warnings, `unwrap_used` denied,
`panic` warned, `unsafe_code` forbidden outside the three audited crates) is
defined in `[workspace.lints]` and detailed in
[coding-standards.md](coding-standards.md). `-D warnings` makes every warning a
hard error — exactly as CI does on the `check` job (Ubuntu/macOS/Windows).

### xtask source lints

Two project-specific lints ship as `xtask` subcommands and are enforced in CI:

```sh
cargo run -p xtask -- lint-spawn      # ban raw tokio::spawn outside spawn_in
cargo run -p xtask -- lint-secrets    # require Secret<T>/#[redact] on secret fields
```

Run them before pushing if you added tasks or secret-bearing structs. See
[coding-standards.md](coding-standards.md) for the rules they enforce.

---

## Coverage

CI's `coverage` job uses `cargo-llvm-cov`. Reproduce locally:

```sh
cargo install cargo-llvm-cov --locked          # one-time (needs llvm-tools-preview)
rustup component add llvm-tools-preview

cargo llvm-cov --workspace --locked            # run + summary to stdout
cargo llvm-cov --workspace --html              # browsable report in target/llvm-cov/html
cargo llvm-cov --workspace --lcov --output-path lcov.info   # the artifact CI uploads
cargo llvm-cov report --summary-only           # text summary from a prior run
```

CI runs instrumented tests with `--no-report`, then emits an `lcov` artifact and
uploads to Codecov (non-blocking). The coverage lane runs on **stable** so the
latest `cargo-llvm-cov` installs cleanly.

---

## The performance gate locally

`perf-gate.yml` builds the release binaries, runs `origin-bench` against
`bench/perf/tasks`, and asserts the worst read-only task `wall_ms` is **≤ 80 ms**.
Reproduce it:

```sh
cargo build --release --locked -p origin-cli -p origin-daemon
cargo run --release --locked -p origin-bench -- run-origin --tasks bench/perf/tasks > result.json
```

The gate considers tasks whose id starts with `01-`/`02-` (the read-only set) and
fails if `max(wall_ms) > 80`. Inspect `result.json` if a hot-path change regresses
the budget, and note any change in the PR.

---

## Fuzzing

The fuzz crate (`crates/origin-daemon/fuzz`) is **excluded from the workspace** and
builds only under nightly Rust (some transitive deps need `edition2024`). The
nightly CI matrix fuzzes the IPC frame validator, the FastCDC chunker, the
Anthropic/OpenAI SSE parsers, the `tool_use_parser`, and the streaming-JSON rkyv
decoder; committed seed corpora live alongside each target.

```sh
rustup toolchain install nightly
cargo install cargo-fuzz --locked

cd crates/origin-daemon/fuzz
cargo +nightly fuzz list                          # available targets
cargo +nightly fuzz run ipc_frame -- -max_total_time=60
```

You normally do not need to run fuzz targets for a routine PR — CI runs the
nightly matrix. Do extend the corpus or add a target when you touch a parser or a
wire boundary.

---

## Supply-chain checks

```sh
cargo install cargo-deny --locked
cargo deny check advisories bans sources    # hard gates (RUSTSEC, rustls-only, crates.io-only)
cargo deny check licenses                    # advisory for now
```

Configuration is the root `deny.toml`. The `bans` check enforces the rustls-only
TLS policy; `sources` enforces a crates.io-only allow-list.

---

## Docs build

```sh
cargo install mdbook --locked --version 0.4.40   # pinned to a 1.83-safe release
mdbook build docs/site                            # the published mdBook
cargo run -p xtask --locked -- manpages --out target/manpages   # clap_mangen manpages
```

The docs site (`docs/site/`) deploys to GitHub Pages from `dev` via `docs.yml`.

---

## Quick reference

| Task | Command |
| --- | --- |
| Build all | `cargo build` |
| Release binaries | `cargo build --release -p origin-cli -p origin-daemon` |
| Run CLI | `cargo run -p origin-cli -- --help` |
| Test all | `cargo test --workspace --locked` |
| Test one crate | `cargo test -p <crate>` |
| Format check | `cargo fmt --all -- --check` |
| Clippy (CI parity) | `cargo clippy --workspace --all-targets --locked -- -D warnings` |
| Coverage | `cargo llvm-cov --workspace` |
| Perf gate | `cargo run --release -p origin-bench -- run-origin --tasks bench/perf/tasks` |
| spawn/secret lints | `cargo run -p xtask -- lint-spawn` / `lint-secrets` |
| Supply chain | `cargo deny check advisories bans sources` |

_Last reviewed against workspace version 0.9.8._
