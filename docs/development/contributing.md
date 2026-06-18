# Contributing

This is the contributor-facing companion to the repository-root
[`CONTRIBUTING.md`](../../CONTRIBUTING.md). It summarizes how we work, the quality
gates your change must clear, and how to file issues and pull requests. When this
page and the root file disagree, the root file wins — it is the source of truth.

By contributing you agree your work is licensed under the project's
[Apache License 2.0](../../LICENSE) (inbound = outbound, per Apache-2.0 §5). There
is **no CLA**. All participants follow the [Code of Conduct](../../CODE_OF_CONDUCT.md).

---

## TL;DR — the contributor's loop

```sh
# 1. Set up (rust-toolchain.toml selects the toolchain automatically)
git clone https://github.com/Kantosaurus/origin && cd origin

# 2. Make a change on a branch off dev, TDD-style (failing test first)

# 3. Run the gates locally — these are exactly what CI runs
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

# 4. Update CHANGELOG.md (## Unreleased) and docs for any behavior change
# 5. Open a PR against dev, fill out the template, paste green test output
```

See [building-and-testing.md](building-and-testing.md) for the full command
catalog (coverage, perf gate, fuzz, per-crate testing, Windows notes).

---

## How we work: brainstorm → plan → TDD → verify

origin ships an opinionated baseline workflow — it is literally embedded in the
daemon's default system prompt (`origin-skills` bundles the *superpowers* skills:
brainstorming, writing/executing plans, TDD, systematic debugging,
verification-before-completion). We ask contributors to follow the same
discipline:

1. **Brainstorm / clarify scope** before writing code, especially for features.
   For anything non-trivial, align on approach first (issue or draft PR).
2. **Write a plan** for multi-step work. Capture non-trivial designs and specs in
   the PR description or a linked tracking issue.
3. **Test-driven development.** Write a failing test first, watch it fail for the
   *right* reason, then make it pass. **Bug fixes must include a regression test**
   that fails without the fix.
4. **Verify before claiming done.** Paste the relevant command output (tests
   green, clippy clean) in the PR rather than asserting success.

---

## Quality gates

Every PR must pass the same checks CI runs (see the workflows under
`.github/workflows/`). Run them locally before pushing.

| Gate | Command | Enforced by |
| --- | --- | --- |
| Formatting | `cargo fmt --all -- --check` | `ci.yml` (`check`) |
| Lints | `cargo clippy --workspace --all-targets --locked -- -D warnings` | `ci.yml` (`check`) |
| Tests | `cargo test --workspace --locked` | `ci.yml` (`check`, `stable`) |
| Coverage | `cargo llvm-cov --workspace` (lcov artifact + Codecov) | `ci.yml` (`coverage`) |
| Performance | read-only task `wall_ms` worst ≤ **80 ms** | `perf-gate.yml` |
| Supply chain | `cargo deny check advisories bans sources` | `audit.yml` |
| Docs build | `mdbook build docs/site` + `xtask manpages` | `docs.yml` |

The `check` job runs on **Ubuntu, macOS, and Windows**. A separate `stable` lane
compiles and tests on current stable Rust (without `-D warnings`, since
pedantic/nursery lints evolve) to catch regressions early.

### Lint policy (strict, enforced as errors in CI)

The workspace turns on clippy **`pedantic`** and **`nursery`** (both `warn`),
**denies `unwrap_used`**, and **warns on `panic`**. In CI, `-D warnings` promotes
every warning to a hard error.

- Prefer `?`, `expect("explains the invariant")`, or explicit error handling over
  `unwrap()`.
- If a lint is genuinely wrong for a spot, scope an `#[allow(clippy::…)]` to the
  **smallest item** and add a one-line justification comment.

### `unsafe` is forbidden

`unsafe_code = "forbid"` is set workspace-wide. The **only** audited exceptions
are `origin-cas`, `origin-tui`, and `origin-ipc` (each re-enables `unsafe` with a
reviewed justification). Do **not** introduce `unsafe` anywhere else. Enforcement
lives in the unsafe-audit gate. See [coding-standards.md](coding-standards.md) for
the full lint table and the audited exceptions.

### Performance is a gate

The `perf-gate.yml` workflow builds `origin-cli`/`origin-daemon` in release and
runs `origin-bench` against `bench/perf/tasks`, asserting read-only tasks stay within
budget (worst `wall_ms` ≤ 80 ms). If your change touches a hot path (IPC, CAS, the
render tick, the agent loop), run the benchmarks locally and call out any
regression in the PR.

---

## MSRV and toolchain

- **MSRV is Rust 1.83** (`rust-version = "1.83"` in `Cargo.toml`, edition 2021).
  Do not rely on language or stdlib features newer than 1.83.
- The repo pins a toolchain in [`rust-toolchain.toml`](../../rust-toolchain.toml)
  (currently channel `1.96.0` with `clippy` + `rustfmt`). `rustup` installs it on
  first build. The pinned toolchain *builds* the code; the MSRV is the *floor* the
  source must remain compatible with.
- Several transitive dependencies are pinned in `Cargo.lock` to stay 1.83-safe
  (e.g. crates that otherwise pull `edition2024`). Keep `--locked` builds green; if
  you must bump a dependency, verify it does not raise the effective MSRV.

---

## Commit & PR conventions

- **Conventional Commits.** Use `type(scope): summary`, e.g.
  `fix(daemon): prevent tool_diff_lines infinite loop on reordered Edit lines` or
  `feat(cli): stall watchdog for the render heartbeat`. Common types: `feat`,
  `fix`, `refactor`, `test`, `docs`, `chore`, `perf`.
- **One logical change per PR.** Keep diffs reviewable; split unrelated work.
- **Branch off `dev`** (the default branch) and open your PR against it.
- **Update docs and `CHANGELOG.md`** (the `## Unreleased` section) when behavior,
  config, or public APIs change.
- Fill out the pull request template (`.github/PULL_REQUEST_TEMPLATE.md`) checklist.

A maintainer reviews; respond to feedback by pushing follow-up commits (we squash
on merge where appropriate). For larger features, open an issue or draft PR early
to align on approach before investing in the full implementation.

If you touch the daemon ↔ CLI boundary, keep the wire contract in mind: the two
processes communicate **only** through `origin-ipc` (rkyv-archived frames). There
is no side channel.

---

## Branching & releases

origin uses a two-branch flow:

- **`dev`** — the default integration/staging branch. All PRs target `dev`; it is
  where features land and are exercised by CI before release. The docs site
  deploys from `dev`.
- **`main`** — the release branch that delivers packages. It is never committed to
  directly; it only advances by merging `dev` once it is stable.

**Cutting a release** is a maintainer task: merge `dev` → `main`, then push a
`vX.Y.Z` tag on `main`. The tag triggers `release.yml`, which builds, signs
(cosign + SLSA provenance), and publishes binaries, the npm package, and the
Homebrew/winget/AUR manifests. Prerelease tags (`-rc`/`-beta`/`-alpha`) publish to
the npm `next` tag and never become `latest`. Full details in
[release-process.md](release-process.md).

---

## Governance

origin is a young, pre-1.0 project with a lightweight governance model
([`GOVERNANCE.md`](../../GOVERNANCE.md)):

- **Maintainer** — currently a single maintainer, [@Kantosaurus](https://github.com/Kantosaurus)
  (Ainsley Woo): reviews/merges PRs, cuts releases, triages, sets direction.
  Security-sensitive ownership is recorded in `.github/CODEOWNERS`.
- **Contributors** — anyone who opens an issue or PR. You do not need to be a
  maintainer to propose changes.
- **Small changes** (bug fixes, docs, tests) merge once CI is green and the gates
  are met. **Larger changes** (new subsystems, public-API or wire-protocol
  changes, dependency additions) should start as an issue or draft PR so the
  approach can be agreed first. The maintainer has final say; the goal is
  consensus.

---

## Filing issues and PRs

- **Bugs / features:** open an issue using the templates in
  `.github/ISSUE_TEMPLATE/` (`bug_report`, `feature_request`, `config`). Include
  `origin --version`, your OS/platform, the provider in use, and relevant logs.
  The daemon writes a human-readable log to `<data-dir>/origin/logs/daemon.log`
  (e.g. `%LOCALAPPDATA%\origin\logs\daemon.log` on Windows) — tail it to capture
  what the daemon was doing.
- **Pull requests:** use `.github/PULL_REQUEST_TEMPLATE.md`. Dependency updates are
  also automated via `.github/dependabot.yml` (cargo + github-actions + npm).
- **Security vulnerabilities:** do **not** open a public issue. Follow
  [`SECURITY.md`](../../SECURITY.md).

---

## Related developer docs

- [building-and-testing.md](building-and-testing.md) — build, test, coverage,
  perf, fuzz, Windows notes.
- [coding-standards.md](coding-standards.md) — lint policy, SPDX headers, error
  enums, `spawn_in`, `Secret<T>`.
- [workspace-layout.md](workspace-layout.md) — the repo tree and crate layering.
- [adding-a-crate.md](adding-a-crate.md) — checklist + templates for a new crate.
- [release-process.md](release-process.md) — versioning and the publish pipeline.

_Last reviewed against workspace version 0.9.8._
