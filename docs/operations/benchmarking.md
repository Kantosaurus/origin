# Benchmarking

origin has three distinct benchmarks, kept separate on purpose:

| | What | Where | Purpose |
|---|---|---|---|
| **SWE-bench Verified A/B** | cross-harness correctness + tokens + $ + wall-clock on a **publishable** dataset | `bench/swe/` (see [`bench/README.md`](../../bench/README.md)) | prove origin is better (or not) vs other agentic harnesses |
| **Regression benchmark** | pass@1 / pass^k + tokens + $ on 20–50 tasks from origin's own past failures | `bench/regression/` (see [`bench/regression/README.md`](../../bench/regression/README.md)) | tell whether a harness change made origin better or worse |
| **Perf gate** | origin's own cold-start / read-only latency probes | `bench/perf/tasks/` + `origin-bench` | internal latency-regression gate (CI) |

The previous origin-specific A/B task set (`bench/tasks`) was retired — the
cross-harness comparison now runs against **publishable benchmarks only**
(SWE-bench Verified).

## SWE-bench Verified A/B (Path B)

The full methodology + run instructions live in [`bench/README.md`](../../bench/README.md).
In short:

```sh
pip install -r bench/swe/requirements.txt
export ANTHROPIC_API_KEY=...  ORIGIN_MODEL=claude-opus-4-8  BENCH_KILL_DAEMON=1

# generate predictions + token/$/wall metrics for each harness (same -n subset!)
python bench/swe/run.py --harness origin --adapter bench/swe/adapters/origin.sh -n 50 --seeds 3
python bench/swe/run.py --harness aider  --adapter bench/swe/adapters/aider.sh  -n 50 --seeds 3

# score with the OFFICIAL swebench harness + aggregate pass@1 / CIs / cost
python bench/swe/evaluate.py --out bench/swe/out --max-workers 8
cat bench/swe/out/leaderboard.md
```

The golden rule: **hold the model constant** across harnesses — you are comparing
scaffolding, not models. Correctness is decided by the canonical `swebench`
evaluator (Docker, hidden `FAIL_TO_PASS`/`PASS_TO_PASS`); `$` uses origin's own
price table (`bench/swe/prices.json`, regenerated via
`cargo run -p xtask -- prices`) so every harness is costed identically.

`bench/swe/adapters/origin.sh` sets `ORIGIN_ALLOW_ALL=1` for the `origin run`
child, which auto-approves every tool in a headless run: run it only inside a
disposable container or VM.

## Regression benchmark

`bench/regression/run.py` runs each task 3–5 times through `origin run` headless
and writes one results JSON: pass@1 (first trial passes), pass^k (all k trials
pass), the per-request token and cache counters, dollars (same price table as
above) and wall time, per task and overall. Model and effort are required
arguments and are recorded in the file. `origin run` exposes no seed, so trials
are paired by index. Protocol, task-file schema and results format:
[`bench/regression/README.md`](../../bench/regression/README.md).

The runner, like the SWE adapter, sets `ORIGIN_ALLOW_ALL=1` for `origin run`, so
run it only inside a disposable container or VM, pinned to the same image and
CPU/memory limits for every run you intend to compare.

Producing and committing the first baseline:

```sh
# 1. validate tasks, confirm each fails untouched, print the commands (no origin calls)
python bench/regression/run.py --model claude-opus-4-8 --effort high --dry-run

# 2. the run
export ANTHROPIC_API_KEY=...  BENCH_KILL_DAEMON=1
python bench/regression/run.py --model claude-opus-4-8 --effort high --trials 3 \
    --out bench/regression/baselines/<date>-<model>-<effort>.json

# 3. commit the results file with the task set; note the origin commit and container limits
git add bench/regression/baselines/ bench/regression/tasks/
```

Statistical note: with 20–50 tasks, a difference of 2–3 points between two
single runs is noise. Treat a change as real only when it is larger than that and
holds on pass^k as well as pass@1, or reproduces in a second run. One task flipping
moves pass@1 by 2–5 points at this size, so also report how many tasks flipped in
each direction (fail→pass, pass→fail) between the paired runs.

Results are rewritten after every trial; an aborted run leaves `"complete": false`
and is not a baseline. Each task lists the test files to restore before its check
(`verify.restore`), so a trial cannot pass by editing or deleting the tests, and a
trial that touched them is marked `tests_modified: true`.

## Perf gate

`origin-bench` drives the two read-only probes in `bench/perf/tasks/` through the
local `origin` binary and the `.github/workflows/perf-gate.yml` workflow asserts
the worst read-only `wall_ms` stays **≤ 80 ms** on every PR:

```sh
cargo build --release -p origin-cli -p origin-daemon
ORIGIN_BIN=target/release/origin \
  cargo run --release -p origin-bench -- run-origin --tasks bench/perf/tasks > result.json
```

`origin run` needs a provider key (e.g. `ANTHROPIC_API_KEY`) to execute real
turns. The perf-gate measures origin's **own** cold-start / agent overhead, not
model time — when it goes red, reproduce locally, inspect `result.json`, and
profile the cold-start path (cross-check the parquet trace ring).

### Performance KPIs

| KPI | Target | Where measured |
|-----|--------|----------------|
| Cold start → first prompt | < 50 ms (proxy: read-only `wall_ms` ≤ 80 ms) | perf-gate over `bench/perf/tasks` |
| RSS | < 1 GiB (supervisor soft budget) | supervisor lifecycle |
| Cache hit rate | ≥ 70% | token planner → `origin_cache_hit_total` + traces (not asserted) |

### Env / args (`origin-bench`)

| Env / Arg | Effect |
|-----------|--------|
| `ORIGIN_BIN` | Path to the `origin` binary (default `target/debug/origin`). |
| `--tasks <dir>` | Task-set root with a `manifest.json` (default `bench/perf/tasks`). |
| `--name` / `--bin` | Contestant name + binary (`run-subprocess`). |
| `--results` / `--out` | Result file/dir in, Markdown out (`report`). |
