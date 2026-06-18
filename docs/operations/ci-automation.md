# CI Automation (Operations)

Operations-focused expansion of the root [`../ci-automation.md`](../ci-automation.md).
That page explains *what* the VCS-automation bots are and *why* they're safe;
this one is the **operator's view**: every workflow in `.github/workflows/`, what
triggers it, which secrets it needs, and how the **quality gates** (build/test,
the ≤80 ms perf gate, supply-chain/`cargo-deny`, coverage, docs) keep `main`
green. The original root doc is unchanged.

> All third-party actions are pinned to a commit SHA (the repo convention) so a
> moved tag cannot change behaviour. Each workflow declares the narrowest
> `permissions:` it needs.

---

## All workflows

| Workflow file | Purpose | Class |
|---|---|---|
| `ci.yml` | fmt + clippy `-D warnings` + tests on 3 OSes, stable compile/test, coverage. | Quality gate |
| `perf-gate.yml` | Cold-start benchmark; **read-only wall_ms ≤ 80 ms** gate. | Quality gate |
| `audit.yml` | `cargo-deny` advisories / bans / sources (+ non-blocking licenses). | Quality gate |
| `docs.yml` | Build mdBook site + manpages; deploy GitHub Pages from `dev`. | Quality gate |
| `release.yml` | Build signed multi-target release binaries on tag `v*`. | Release |
| `fuzz.yml` | Fuzz targets. | Quality gate |
| `scorecard.yml` | OSSF scorecard. | Supply chain |
| `unsafe-audit.yml` | `unsafe` usage audit. | Supply chain |
| `origin-mention.yml` | Reply to `@origin <instruction>` in issue/PR comments. | VCS bot |
| `origin-pr-review.yml` | Review PRs on open / update. | VCS bot |
| `origin-issue-triage.yml` | Classify + label new issues. | VCS bot |
| `origin-schedule.yml` | Scheduled / on-demand maintenance → opens a PR. | VCS bot |
| `.gitlab-ci.yml` (repo root) | GitLab MR review parity (manual, opt-in). | VCS bot |

---

## Triggers & permissions

### Quality gates

| Workflow | Triggers | Permissions | Notes |
|---|---|---|---|
| `ci.yml` | push & PR to `dev`/`main` | `contents: read` | Matrix: ubuntu/macos/windows on toolchain **1.96.0** (clippy+rustfmt); a second job on **stable** (compile+test only, no `-D warnings`); a coverage job. 60-min timeouts (Windows is slowest). `concurrency` cancels in-progress. |
| `perf-gate.yml` | PR to `dev`/`main`, `workflow_dispatch` | `contents: read` | Builds `origin-cli` + `origin-daemon` release, runs `origin-bench`, asserts the gate. 30-min timeout. |
| `audit.yml` | push & PR to `dev`/`main`, **daily cron 07:00 UTC**, dispatch | `contents: read` | `cargo deny check advisories bans sources` is a hard gate; `licenses` is non-blocking (`continue-on-error`). |
| `docs.yml` | push to `dev`/`main`, PR to `dev` | build: `contents: read`; deploy: `pages: write`, `id-token: write` | mdBook pinned to 0.4.40 (MSRV-safe); deploys Pages only from `dev`. |
| `release.yml` | tag `v*`, dispatch | `contents: write`, `id-token: write`, `attestations: write` | Multi-target matrix (linux gnu x64/arm64, macOS arm64, windows msvc x64/arm64). Build attestations. |

### VCS automation bots

| Workflow | Triggers | Permissions | Guard / opt-out |
|---|---|---|---|
| `origin-mention.yml` | `issue_comment`, `pull_request_review_comment` (created) | `contents: read`, `issues: write`, `pull-requests: write` | `if: contains(comment.body, '@origin')`; concurrency per thread. |
| `origin-pr-review.yml` | `pull_request` (opened/synchronize), dispatch (`mode`) | `contents: read`, `pull-requests: write` | `[skip origin]` in PR title opts out. |
| `origin-issue-triage.yml` | `issues` (opened) | `contents: read`, `issues: write` | Label validated against a fixed allow-list. |
| `origin-schedule.yml` | `workflow_dispatch` (cron commented out) | `contents: write`, `pull-requests: write` | Opens a PR only when the tree changed. |
| `.gitlab-ci.yml` | `merge_request_event`, `when: manual` | (CI variables) | Manual/opt-in. |

---

## Required secrets

| Secret | Where | Used by | Notes |
|---|---|---|---|
| `ANTHROPIC_API_KEY` | GitHub repo secrets / GitLab CI vars | every bot that runs `origin run` | Provider key; mask + protect. Not needed for `gh`/`curl`-only steps. |
| `GITHUB_TOKEN` | provided automatically | mention, PR review, triage, schedule | Scoped per-workflow via `permissions:`; drives `gh`. |
| `GITLAB_TOKEN` | GitLab CI/CD vars | `.gitlab-ci.yml` | Project/group token with `api` scope (the default `CI_JOB_TOKEN` can't write notes). |
| `CODECOV_TOKEN` | GitHub repo secrets | `ci.yml` coverage | Uploads `lcov.info`; `fail_ci_if_error: false`. |

---

## The quality gates in detail

### Build, lint & test (`ci.yml`)

```sh
# Reproduce locally:
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

The pinned-toolchain (`1.96.0`) matrix runs all three on **ubuntu, macos, and
windows**. A separate **stable** job runs only compile+test (clippy's
pedantic/nursery lints evolve, so `-D warnings` is *not* applied on stable to
avoid flaky breakage). Both honour `--locked` so a stale `Cargo.lock` fails CI.

### Perf gate — read-only ≤ 80 ms (`perf-gate.yml`)

The perf gate is the GA acceptance proxy for "cold start to first prompt is
fast." It builds the release binaries, runs `origin-bench`, and asserts:

```sh
cargo build --release --locked -p origin-cli -p origin-daemon
cargo run --release --locked -p origin-bench -- run-origin --tasks bench/tasks > result.json
# Gate (paraphrased): the WORST wall_ms across read-only tasks (ids 01-/02-)
# must be <= 80 ms, else the job fails.
```

The 80 ms ceiling is a proxy for the spec's <50 ms cold-start target. The
cache-hit-rate target (≥ 70%) is measured by the token planner and surfaced in
traces/metrics but is **not** asserted here. See
[`benchmarking.md`](./benchmarking.md) for the KPIs and task manifest.

### Supply chain — `cargo-deny` (`audit.yml`)

```sh
cargo install cargo-deny --locked
cargo deny check advisories bans sources   # hard gate
cargo deny check licenses                  # advisory (non-blocking)
```

- **advisories** — RUSTSEC vulnerability advisories.
- **bans** — dependency bans, including the **rustls-only** TLS policy (no
  OpenSSL anywhere in the graph — relevant to the QUIC+mTLS transport).
- **sources** — crates.io-only source allow-list.
- **licenses** — advisory until the allow-list is tuned; flip off
  `continue-on-error` to make it a gate.

Runs on every push/PR **and** daily at 07:00 UTC so a newly-published advisory is
caught even without a code change.

### Coverage (`ci.yml` coverage job)

```sh
cargo install cargo-llvm-cov --locked
cargo llvm-cov --workspace --locked --no-report
cargo llvm-cov report --lcov --output-path lcov.info
cargo llvm-cov report --summary-only >> "$GITHUB_STEP_SUMMARY"
```

Uploads `lcov.info` as an artifact and to Codecov (non-blocking). Uses **stable**
so the latest `cargo-llvm-cov` installs cleanly.

### Docs (`docs.yml`)

```sh
cargo install mdbook --locked --version 0.4.40   # MSRV(1.83)-safe pin
mdbook build docs/site
cargo run -p xtask --locked -- manpages --out target/manpages
```

Builds the site + manpages; publishes GitHub Pages from `dev` only (the deploy
job has the extra `pages`/`id-token` permissions; `concurrency: pages` never
cancels an in-flight deploy).

---

## How each bot behaves operationally

- **`@origin` mentions** — the comment body is **untrusted**: it's passed via
  `env:` (`COMMENT_BODY` / `ORIGIN_INSTRUCTION`) and read as a quoted shell
  variable, never interpolated into a command, so `@origin $(rm -rf /)` is inert.
  Installs origin via `npm install -g @kantosaurus/origin`, runs
  `origin run "$ORIGIN_INSTRUCTION"`, posts back with `gh pr comment` /
  `gh issue comment`. Concurrency is keyed per thread.
- **PR review** — full-history checkout, diffs `origin/<base>...HEAD`, posts a
  grouped review (Blocking / Should-fix / Nit). `mode` input `balanced` (default)
  or `strict`. `[skip origin]` in the PR title opts a PR out.
- **Issue triage** — classifies into `bug` / `feature` / `question` /
  `documentation`; the model's first output line is **sanitized to that
  allow-list** (falling back to `needs-triage`) before `gh issue edit
  --add-label`.
- **Scheduled maintenance** — defaults to `workflow_dispatch`; the daily cron is
  present but **commented out** so nothing fires unexpectedly.
  `peter-evans/create-pull-request` opens a PR only when the tree changed.
- **GitLab MR review** — a single `origin_review` job, `when: manual`, gated to
  MR pipelines; POSTs an MR note via `GITLAB_TOKEN`.

---

## Operating notes & runbook

| Situation | Action |
|---|---|
| A bot job fails with auth errors | Confirm `ANTHROPIC_API_KEY` (and `GITLAB_TOKEN` for GitLab) is set + unexpired in repo secrets / CI vars. |
| Perf gate red | Reproduce locally with the two commands above; inspect `result.json` worst `wall_ms`; profile cold start, not just the model. |
| `cargo-deny` advisories red | Read the RUSTSEC id; bump or patch the offending crate; if a false positive, add a scoped exception in `deny.toml`. |
| Coverage step "killed" mid-run | Instrumented builds are slow; the job already uses a 60-min timeout — re-run, don't lower coverage. |
| Pages didn't update | Pages deploys only from `dev`; PRs build but don't deploy. |
| Want to silence a bot on one PR | Add `[skip origin]` to the PR title (review bot). |
| Want maintenance on a schedule | Uncomment the cron in `origin-schedule.yml` deliberately, with review. |

---

_Last reviewed against workspace version 0.9.8._
