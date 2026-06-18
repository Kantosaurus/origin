# `bench/` — origin benchmarking

Two independent things live here:

- **`swe/` — SWE-bench Verified A/B** (Path B): the *publishable* cross-harness
  benchmark that proves origin is better (or not) on **correctness + tokens + $ +
  wall-clock**, on the **same model**, against other agentic coding harnesses.
- **`perf/` — internal latency gate**: origin's own cold-start / read-only latency
  probes, asserted in CI by `.github/workflows/perf-gate.yml`. Not an A/B; kept
  separate from the publishable benchmark.

The previous origin-specific A/B task set (`bench/tasks/`) was removed — we
benchmark against **publishable** datasets only.

---

## The one rule that makes the result credible

**Hold the model constant.** A harness is scaffolding (context management, tool
use, swarm, token efficiency) around a model. Point origin *and every competitor*
at the **same model via the same API** (e.g. `claude-opus-4-8`). A different
model means you measured *models*, not *harnesses* — and proved nothing.

origin's honest, provable edge is **efficiency**: *same-or-better solve rate at
fewer tokens / lower $ / lower latency*. The harness reports all of those so you
can make exactly that claim.

---

## `swe/` — SWE-bench Verified A/B

### What it does

1. **generate** (`run.py`) — per task × harness × seed: lay down a clean
   `<repo>@<base_commit>` checkout, run the harness headless via an **adapter**,
   capture the candidate patch + token/wall metrics → `out/predictions/<h>.jsonl`
   + `out/metrics/<h>.jsonl`.
2. **score + aggregate** (`evaluate.py`) — score the patches with the **official
   `swebench` evaluator** (Docker per task, hidden `FAIL_TO_PASS`/`PASS_TO_PASS`
   tests — *we never decide pass/fail ourselves*), join with the metrics, and
   report per harness: **pass@1 with 95% bootstrap CIs**, mean tokens, **$**
   (origin's price table), and wall-clock → `out/leaderboard.{md,json}`.

### Prerequisites

- **Docker** (the official scorer runs each task in its container) — Linux host.
- **Python 3.10+**: `pip install -r bench/swe/requirements.txt`
- The **harness binaries** on PATH: `origin` (this repo), plus any competitors
  (`aider`, `claude-code`, …).
- A provider key for the shared model, e.g. `ANTHROPIC_API_KEY`.

### Run it

```bash
pip install -r bench/swe/requirements.txt

# 1) generate predictions + metrics for each harness (same -n subset for all!)
export ANTHROPIC_API_KEY=...   ORIGIN_MODEL=claude-opus-4-8   BENCH_KILL_DAEMON=1
python bench/swe/run.py --harness origin --adapter bench/swe/adapters/origin.sh -n 50 --seeds 3
python bench/swe/run.py --harness aider  --adapter bench/swe/adapters/aider.sh  -n 50 --seeds 3

# 2) score with the official harness + aggregate into a leaderboard
python bench/swe/evaluate.py --out bench/swe/out --max-workers 8
cat bench/swe/out/leaderboard.md
```

Start with `-n 50 --seeds 3` for a fast, statistically-meaningful signal; scale to
the full 500 × ≥3 seeds for a publishable number. The `-n` slice is a *stable*
sorted prefix, so every harness solves the **same** tasks.

### Adding a competitor

Copy `swe/adapters/_template.sh` and fill in the harness invocation. The contract:

```
adapter.sh <repo_dir> <issue_file> <metrics_out_json>
  stdout              -> the candidate patch (a `git diff`)
  <metrics_out_json>  -> {input_tokens,output_tokens,cache_read,cache_creation,wall_s,model}
```

`aider.sh` is a worked example. **Cost is computed from the token counts** by
`cost.py` using origin's own rates (`prices.json`), so every harness is costed
identically — adapters only need to report honest token counts.

### Cost = origin's price table (single source of truth)

`swe/prices.json` is generated from the `origin-cost` crate:

```bash
cargo run -p xtask -- prices > bench/swe/prices.json
```

`cost.py` mirrors `origin_cost::price_for` (normalize model id → longest-prefix
match), so the `$` column is exactly what origin itself would charge.

### Files

```
bench/swe/
  run.py            generate predictions + metrics for one harness
  evaluate.py       official swebench scoring + pass@1/CI/cost aggregation
  cost.py           price token usage with origin's rates
  prices.json       origin-cost price table (regen: cargo run -p xtask -- prices)
  requirements.txt  swebench + datasets
  adapters/
    origin.sh       origin headless adapter (real)
    aider.sh        aider competitor (worked example)
    _template.sh    contract skeleton for new competitors
```

### Caveats / honest notes

- This is the **harness setup**; an actual run needs Docker, the dataset download,
  per-task Docker images, and API spend — run it on a Linux bench host.
- The official `swebench` report path/format shifts across versions; `evaluate.py`
  globs for the `resolved_ids` report and warns if it can't find one — adjust the
  glob for your `swebench` version if needed.
- origin spawns a per-workspace daemon; set `BENCH_KILL_DAEMON=1` so daemons don't
  accumulate across hundreds of tasks. `run.py` is sequential by default — shard
  by `--ids` across machines for scale.
- Lead your published claim with the **efficiency win** (tokens/$/latency at equal
  solve rate); only claim a higher solve rate if the CIs separate.

---

## `perf/` — internal latency gate

`perf/tasks/` holds the two read-only probes (`01-read-and-summarize`,
`02-grep-and-explain`) the `perf-gate` CI workflow runs through `origin-bench` to
assert origin's read-only wall-clock stays under budget. This is origin's own
regression gate, not a cross-harness comparison.
