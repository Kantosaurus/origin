# CI / VCS automation

`origin` ships first-party CI automation so the headless agent
(`origin run "<prompt>" [--json]`) can drive everyday repository chores —
answering `@origin` mentions, reviewing pull/merge requests, triaging issues, and
running scheduled maintenance. It is the `origin` analogue of claude-code's
`@claude` GitHub Action, gemini-cli's `run-gemini-cli`, and the
kilocode/opencode PR-review + issue-triage bots and GitLab integration.

These workflows are the thin CI shell around two crates:

- **`origin-review`** — confidence-scored review primitives: dedup of repeat
  findings, ranking by confidence, and `origin-review::triage` (Bug / Feature /
  Question / Docs) with `similarity()` to flag duplicate issues.
- **`origin-schedule`** — cron/recurring job scheduling, the engine behind the
  scheduled-maintenance workflow.

## Files

| File | Purpose |
| --- | --- |
| [`.github/workflows/origin-mention.yml`](../.github/workflows/origin-mention.yml) | Reply to `@origin <instruction>` in issue / PR comments. |
| [`.github/workflows/origin-pr-review.yml`](../.github/workflows/origin-pr-review.yml) | Review PRs on open / update. |
| [`.github/workflows/origin-issue-triage.yml`](../.github/workflows/origin-issue-triage.yml) | Classify + label new issues. |
| [`.github/workflows/origin-schedule.yml`](../.github/workflows/origin-schedule.yml) | Scheduled / on-demand maintenance that opens a PR. |
| [`.gitlab-ci.yml`](../.gitlab-ci.yml) | GitLab MR review parity (manual, opt-in). |

## Required secrets

| Secret | Where | Notes |
| --- | --- | --- |
| `ANTHROPIC_API_KEY` | GitHub repo secrets / GitLab CI variables | Provider key for `origin run`. Mask + protect it. |
| `GITHUB_TOKEN` | Provided automatically by GitHub Actions | Used by the `gh` CLI to post comments / labels. Scoped per-workflow via `permissions:`. |
| `GITLAB_TOKEN` | GitLab CI/CD variables | Project/group access token with the `api` scope so the job can POST an MR note (the default `CI_JOB_TOKEN` cannot write notes). |

No `ANTHROPIC_API_KEY` is needed for the parts that only call `gh`/`curl`, but
every workflow that runs `origin run` requires it.

## How each piece works

### `@origin` mentions

Trigger: `issue_comment` and `pull_request_review_comment` (type `created`).
A guard (`if: contains(github.event.comment.body, '@origin')`) keeps the runner
idle unless the comment actually mentions the bot. The workflow extracts the text
after the first `@origin` token, runs `origin run "<instruction>"`, and posts the
output back with `gh issue comment` / `gh pr comment`. A concurrency group keyed
on the issue/PR number serializes overlapping mentions.

### PR review

Trigger: `pull_request` (`opened`, `synchronize`), plus manual
`workflow_dispatch` with a `mode` input (`balanced` default, or `strict`).
It checks out full history (`fetch-depth: 0`), diffs against
`origin/${{ github.base_ref }}...HEAD`, asks `origin run` for a grouped review
(Blocking / Should-fix / Nit), and posts one summary comment. Add `[skip origin]`
to the PR title to opt a PR out. The `origin-review` crate supplies the
confidence-scored dedup/triage logic a richer inline-comment bot can build on.

### Issue triage

Trigger: `issues` (`opened`). `origin run` classifies the issue into
`bug` / `feature` / `question` / `documentation`; the first output line is
sanitized to that known set (falling back to `needs-triage`) and applied with
`gh issue edit --add-label`. A comment records the rationale and notes that
`origin-review::triage` does the classification and `similarity()` can dedup
duplicates.

### Scheduled maintenance

Trigger: `workflow_dispatch` by default (with a `task` input); a daily
`cron: "0 6 * * *"` line is included but **commented out** so nothing fires
unexpectedly. `origin run` performs a maintenance task, and
`peter-evans/create-pull-request` opens a PR with any changes (it no-ops on a
clean tree). This is the CI surface of the `origin-schedule` crate, paralleling
claude `/schedule`, kilocode Triggers, and opencode cron.

### GitLab MR review

`.gitlab-ci.yml` defines a single `origin_review` job, gated to
`merge_request_event` pipelines and set to `when: manual` so it is opt-in. It
installs origin, diffs against `CI_MERGE_REQUEST_TARGET_BRANCH_NAME`, runs
`origin run`, and POSTs a note to
`${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/merge_requests/${CI_MERGE_REQUEST_IID}/notes`
using `GITLAB_TOKEN`.

## Security notes

- **Untrusted input via env, never inline.** Comment bodies, PR titles, diffs,
  and issue text are all attacker-controllable. They are passed through `env:`
  and read by the shell as quoted variables (or written to a file consumed as
  data) — never interpolated into a command with `${{ ... }}`. This prevents
  shell/script injection such as `@origin $(rm -rf /)`.
- **Least-privilege permissions.** Each workflow declares the narrowest
  `permissions:` it needs: mention → `contents: read, issues: write,
  pull-requests: write`; PR review → `contents: read, pull-requests: write`;
  triage → `contents: read, issues: write`; schedule → `contents: write,
  pull-requests: write` (it pushes a branch).
- **Opt-in scheduling and review.** The schedule cron is commented out and
  defaults to manual dispatch; the GitLab job is `when: manual`; PRs can opt out
  with `[skip origin]`.
- **Pinned actions.** Third-party actions are pinned to a commit SHA (matching
  the repo's existing `ci.yml` convention) so a moved tag can't change behavior.
- **Output sanitization.** Triage validates the model's label against a fixed
  allow-list before it touches the repo.
