# Contributing to origin

Thanks for your interest in improving **origin**! This document covers how to
set up a dev environment, the quality gates your change must clear, and how to
get a pull request merged.

By contributing, you agree that your contributions are licensed under the
project's [Apache License 2.0](LICENSE) (inbound = outbound, per Apache-2.0 §5).
There is **no CLA**. All participants are expected to follow the
[Code of Conduct](CODE_OF_CONDUCT.md).

---

## Development setup

**Prerequisites**

- **Rust 1.83** — this is the MSRV and the pinned toolchain. It is selected
  automatically by [`rust-toolchain.toml`](rust-toolchain.toml); `rustup` will
  install it on first build. Do not rely on features newer than 1.83.
- *Optional:* Node ≥ 18 (browser sidecar), and a provider API key
  (e.g. `ANTHROPIC_API_KEY`) for anything that hits a live model.

**Build & test**

```sh
cargo build                 # whole workspace
cargo test --workspace      # unit + integration tests
cargo run -p origin-cli --  --help
```

The CLI (`origin-cli`) supervises the daemon (`origin-daemon`) for you; they
communicate only through `origin-ipc`. If you're touching one side of that
boundary, keep the wire contract (rkyv-archived frames) in mind.

---

## Quality gates

Every PR must pass the same checks CI runs (see [`.github/workflows/`](.github/workflows/)).
Run them locally before pushing:

```sh
cargo fmt --all -- --check                          # formatting (rustfmt.toml)
cargo clippy --workspace --all-targets -- -D warnings   # see lints below
cargo test --workspace                              # tests
```

**Lints are strict and enforced as errors in CI.** The workspace turns on
clippy `pedantic` and `nursery`, **denies `unwrap_used`**, and warns on `panic`.
Prefer `?`, `expect("explains the invariant")`, or explicit error handling over
`unwrap()`. If a lint is genuinely wrong for a spot, scope an
`#[allow(clippy::...)]` to the smallest item and add a one-line justification.

**`unsafe` is forbidden** workspace-wide (`unsafe_code = "forbid"`). The only
audited exceptions are `origin-cas`, `origin-tui`, and `origin-ipc`; the
[`unsafe-audit`](.github/workflows/unsafe-audit.yml) workflow enforces this. Do
not introduce `unsafe` elsewhere.

**Performance is a gate.** The [`perf-gate`](.github/workflows/perf-gate.yml)
workflow asserts read-only tasks stay within budget (≤ 80 ms wall). If your
change touches a hot path (IPC, CAS, the render tick, the agent loop), run the
benchmarks in `origin-bench` and call out any regression in the PR.

---

## How we work: brainstorm → plan → TDD → verify

origin ships an opinionated baseline workflow (it's literally in the daemon's
default system prompt). We ask contributors to follow the same discipline:

1. **Brainstorm / clarify scope** before writing code, especially for features.
2. **Write a plan** for multi-step work; capture non-trivial designs and specs
   in the pull request description or a linked tracking issue.
3. **Test-driven development.** Write a failing test first, watch it fail for the
   right reason, then make it pass. Bug fixes **must** include a regression test
   that fails without the fix.
4. **Verify before claiming done.** Paste the relevant command output (tests
   green, etc.) in the PR rather than asserting success.

---

## Commit & PR conventions

- **Conventional Commits.** Use `type(scope): summary`, e.g.
  `fix(daemon): prevent tool_diff_lines infinite loop on reordered Edit lines`
  or `feat(cli): stall watchdog for the render heartbeat`. Common types:
  `feat`, `fix`, `refactor`, `test`, `docs`, `chore`, `perf`.
- **One logical change per PR.** Keep diffs reviewable; split unrelated work.
- **Branch** off `dev` (the default branch) and open your PR against it.
- **Update docs and `CHANGELOG.md`** (the `## Unreleased` section) when behavior,
  config, or public APIs change.
- Fill out the [pull request template](.github/PULL_REQUEST_TEMPLATE.md) checklist.

A maintainer will review; please respond to feedback by pushing follow-up commits
(we squash on merge where appropriate). For larger features, open an issue or a
draft PR early to align on approach before investing in the full implementation.

---

## Reporting bugs & requesting features

- **Bugs / features:** open an issue using the templates in
  [`.github/ISSUE_TEMPLATE/`](.github/ISSUE_TEMPLATE/). Include `origin --version`,
  your OS/platform, the provider in use, and relevant logs. The daemon writes a
  human-readable log to `<data-dir>/origin/logs/daemon.log` (e.g.
  `%LOCALAPPDATA%\origin\logs\daemon.log` on Windows) — tail it to capture what
  the daemon was doing.
- **Security vulnerabilities:** do **not** open a public issue. Follow
  [SECURITY.md](SECURITY.md).

Thanks again — happy hacking!
