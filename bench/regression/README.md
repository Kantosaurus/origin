# `bench/regression/` — origin's own regression benchmark

A small, fixed task set run through `origin run` headless, used to tell whether a
harness change made origin better or worse. Unlike `bench/swe/` it is not a
cross-harness comparison: it compares origin against an earlier origin, at the same
model and effort, on tasks drawn from origin's own past failures.

> **Run it only inside a disposable container or VM.** The runner sets
> `ORIGIN_ALLOW_ALL=1` for the `origin run` child, which auto-approves every tool
> (shell commands included) in a headless run. Never point it at a machine or
> checkout you care about. `BENCH_KILL_DAEMON=1` runs `pkill -f origin-daemon`,
> which kills every origin daemon on the host, another reason to keep the run alone
> in its container.

## Protocol

- **Tasks:** 20–50, each drawn from a real failure of origin (a bug report, a
  transcript where the agent went wrong, a fix on this branch). The two tasks in
  `tasks/` are worked examples of the format, not the set.
- **Trials:** 3–5 per task (`--trials`). Every trial starts from the same
  untouched repo.
- **Paired trials:** `origin run` has no seed option, so runs cannot be made
  deterministic. Trials are paired by index instead: trial *i* of the baseline is
  compared with trial *i* of the candidate. The results file records `"seed": null`
  and the trial index.
- **Pinned:** model and effort are required arguments (no defaults) and are written
  into the results file. Pin the container too (same image, same
  `docker run --cpus N --memory M` every time); the runner records the platform and
  CPU count it saw. Compare two results files only when all of these match.
- **Reported:** pass@1 (share of tasks whose first trial passes), pass^k (share of
  tasks where all k trials pass), per-trial tokens (input, uncached input, cache
  read, cache write, output, request count, prompt-prefix size), dollars and wall
  time, per task and overall.

### Reading the numbers

With 20–50 tasks, a difference of 2–3 points between two single runs is noise.
Treat a change as real only when it exceeds that and holds on pass^k as well as
pass@1, or when it reproduces in a second run. At this size one task flipping moves
pass@1 by 2–5 points, so alongside the rates report how many tasks flipped in
each direction between the paired runs (fail→pass and pass→fail, by task id and
trial index). Token and dollar differences are less noisy than pass rates, but check
them across all trials, not one.

## Task files

One directory per task under `tasks/`, holding `task.json` and (for a fixture task)
the starting repo:

```
tasks/<id>/task.json
tasks/<id>/repo/...        # fixture tasks only
```

```json
{
  "id": "fix-in-other-file",
  "repo": { "fixture": "repo" },
  "prompt": "test_slug.py fails: ... Find the root cause and fix it ...",
  "verify": { "cmd": ["{python}", "-m", "unittest", "-q"], "expect_exit": 0, "restore": ["test_slug.py"] },
  "timeout_s": 900
}
```

| Field | Meaning |
|---|---|
| `id` | Unique task name (use the directory name). |
| `repo.fixture` | Starting repo: a directory, relative to `task.json`. It is copied and given one base git commit. |
| `repo.git` + `repo.commit` | Starting repo instead: a git URL and the commit to check out. Use exactly one of `fixture` / `git`. |
| `prompt` | The text passed to `origin run`. |
| `verify.cmd` | The check, as an argv list, run in the repo after origin finishes. `{python}` is replaced with the runner's own Python. |
| `verify.expect_exit` | The trial passes iff the check exits with this status (a timed-out check fails). |
| `verify.restore` | Optional, but set it: files or directories (relative to the repo) put back exactly as in the untouched repo before the check runs, so the agent cannot pass by weakening or deleting the tests. A restored directory is replaced whole. If any of them differed from what the agent left, the trial row says `tests_modified: true`. |
| `timeout_s` | Cap for `origin run` (and separately for the check). On timeout the whole process tree is killed. A timed-out run still gets checked. |

Pre-flight: before the first trial of the run, every task is set up (fixture copied,
or repo cloned) and its check is run on the untouched repo; a task whose check
already passes, whose setup fails, or whose `verify.restore` names a missing path
is rejected and nothing is run. A task the agent cannot fail measures nothing.
`--dry-run` never touches the network, so it does this check for fixture tasks only;
for a git task it validates the fields and prints the clone command it would run.

The two examples are offline fixtures with the shapes of real failures this plan
fixed:

- `two-file-flag`: the change needs edits in two files (a library function and the
  CLI that exposes it).
- `fix-in-other-file`: the prompt names the failing test and the file it exercises,
  but the root cause is in a shared helper in a third file; a fix in the named file
  leaves another test red.

## Running

Needs `origin` on PATH (or `--origin <path>`), `git`, Python 3.10+ (standard
library only) and a provider key for the model, e.g. `ANTHROPIC_API_KEY`.

```bash
# validate the task files, check each fixture task fails untouched, print the commands;
# no origin calls, no network (git tasks are not cloned)
python bench/regression/run.py --model claude-opus-4-8 --effort high --dry-run

# the run itself (inside the disposable container)
export ANTHROPIC_API_KEY=...  BENCH_KILL_DAEMON=1
python bench/regression/run.py --model claude-opus-4-8 --effort high --trials 3 \
    --out bench/regression/baselines/<date>-<model>-<effort>.json

# the metric arithmetic, offline
python bench/regression/run.py --self-test
```

`--effort` is one of `fast`, `low`, `medium`, `high`, `max`. `BENCH_KILL_DAEMON=1`
kills the per-workspace `origin-daemon` after each trial, as in `bench/swe/`.
Dollars come from `bench/swe/prices.json` via `bench/swe/cost.py`, the same table
the SWE-bench harness uses; a model missing from it costs `0`.

## Producing and committing the first baseline

1. Grow `tasks/` to 20–50 tasks from real failures (keep the two examples or drop
   them).
2. In the pinned container, at the origin commit you want as the reference, run the
   dry run, then the run above with `--trials 3` (or up to 5).
3. Commit the results file under `bench/regression/baselines/`, named with the date,
   model and effort, together with the task set it was run on. Note the origin
   commit and the container limits in the commit message.
4. For a later change, rerun with the same model, effort, trials, task set and
   container, and compare pass@1, pass^k, tokens and dollars against the baseline
   with the noise rule above.

The results JSON is rewritten after every trial. If the run aborts (an error,
Ctrl-C), it still holds every finished trial and says `"complete": false`; only a
file with `"complete": true` is a baseline.

```
{ model, effort, trials_per_task, complete, origin_version, seed_note, host{platform,cpu_count}, finished_at,
  summary{tasks, trials, pass@1, pass^k, input_tokens, uncached_input_tokens, cache_read_tokens,
          cache_write_tokens, output_tokens, requests, cost_usd, wall_s},
  tasks[{id, pass@1, pass^k, passes,
         trials[{trial, seed, passed, verify_exit, verify_timed_out, tests_modified,
                 origin_exit, timed_out, origin_stderr_tail, wall_s,
                 input_tokens, uncached_input_tokens, cache_read_tokens, cache_write_tokens,
                 output_tokens, requests, prompt_prefix_approx_tokens, cost_usd}]}] }
```

`origin_exit` is `null` when origin timed out; `origin_stderr_tail` holds the last
500 bytes of its stderr whenever it did not exit 0.

Token counts are the sums of the `request_usage` events in
`origin run --output-format stream-json` (one per provider request; its
`input_tokens` is uncached input). `input_tokens` in the results is uncached + cache
read + cache write. `prompt_prefix_approx_tokens` is the system prompt plus tool
schemas, from the `prompt_prefix` event (bytes / 4). An origin build without those
events reports `requests: 0` and zero tokens; the runner warns on stderr for every
trial where origin exited 0 with no requests, and `summary.requests` makes a
zero-token baseline obvious.
