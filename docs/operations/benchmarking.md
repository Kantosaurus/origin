# Benchmarking

origin has two distinct benchmarks, kept separate on purpose:

| | What | Where | Purpose |
|---|---|---|---|
| **SWE-bench Verified A/B** | cross-harness correctness + tokens + $ + wall-clock on a **publishable** dataset | `bench/swe/` (see [`bench/README.md`](../../bench/README.md)) | prove origin is better (or not) vs other agentic harnesses |
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
