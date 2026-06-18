# Benchmarking

Operational guide to **`origin-bench`** — the harness that measures `origin`
against other agent CLIs and feeds the CI **perf gate**. This page covers what it
measures, the task manifest, how to run it, the performance KPIs, and how the CI
gate uses the results.

> Cross-links: the gate workflow in [`ci-automation.md`](./ci-automation.md);
> live perf signals (cache hit rate, token usage) in
> [`observability-runbook.md`](./observability-runbook.md).

---

## What `origin-bench` measures

`origin-bench` drives a **fixed task set** through `origin` (and, for comparison,
other coding-agent CLIs) and records per-task metrics. Each run yields a
`TaskResult`:

| Field | Meaning |
|---|---|
| `contestant` | `origin`, or a named comparison CLI (CC / jcode / opencode …). |
| `task_id` | Which task. |
| `input_tokens` / `output_tokens` | Tokens billed (summed across turns). |
| `wall_ms` | End-to-end wall-clock latency for the task. |
| `tool_calls` | Number of tool invocations. |
| `passed` | All budgets met (see below). |

A task **passes** when *all* hold: the subprocess exited 0, **and**
`wall_ms ≤ max_turn_latency_ms`, **and** `input_tokens ≤ max_input_tokens`,
**and** `output_tokens ≤ max_output_tokens`, **and**
`tool_calls ≤ expected_tool_calls_max`. So the harness scores both *correctness*
(exit status) and *efficiency* (latency + token + tool budgets) together.

For repeated runs, `origin-bench::reliability` adds **multi-sample** metrics over
*K* independent runs of the same task — `pass@k` (≥ 1 of k passes), `pass^k` (all
k pass), and a `0..=1` **flakiness** score peaking at a 0.5 pass rate — plus
cheap substring **failure triage**. Single runs give a point estimate; reliability
gives variance.

---

## The task manifest (`bench/tasks`)

The runner loads `bench/tasks/manifest.json`, then every task JSON it references:

```json
{ "version": 1,
  "tasks": [
    "01-read-and-summarize.json", "02-grep-and-explain.json",
    "03-edit-trivial.json", "04-edit-multifile.json",
    "05-bash-build.json", "06-mcp-readonly.json",
    "07-skill-injection.json", "08-swarm-refactor.json" ] }
```

There is also a `bench/tasks/polyglot/` set (Rust, TS, Python, Go, Java tasks).
Each task file declares a prompt and budgets:

```json
{
  "id": "01-read-and-summarize",
  "prompt": "Read README.md and summarize in 2 sentences.",
  "expected_tools_min": ["Read"],
  "expected_tool_calls_max": 4,
  "max_turn_latency_ms": 5000,
  "max_input_tokens": 4000,
  "max_output_tokens": 1000
}
```

| Task | Exercises |
|---|---|
| `01-read-and-summarize` | Read + summarize (read-only; perf-gate input). |
| `02-grep-and-explain` | Grep + explanation (read-only; perf-gate input). |
| `03-edit-trivial` | A single-file edit. |
| `04-edit-multifile` | A cross-file edit. |
| `05-bash-build` | Build via Bash. |
| `06-mcp-readonly` | A read-only MCP tool. |
| `07-skill-injection` | Skill injection. |
| `08-swarm-refactor` | Multi-agent swarm refactor. |

The **read-only** tasks (ids prefixed `01-`/`02-`) are the ones the CI perf gate
keys on, because they isolate the agent's own overhead from heavy I/O or builds.

---

## How to run it

### List the set

```sh
cargo run -p origin-bench -- list
```

### Run origin against the task set

```sh
# Build the binary first; the runner shells out to it.
cargo build --release -p origin-cli -p origin-daemon
# Point the runner at the built binary (defaults to target/debug/origin):
ORIGIN_BIN=target/release/origin \
  cargo run --release -p origin-bench -- run-origin --tasks bench/tasks > result.json
```

`run-origin` invokes `origin run --json --prompt "<task prompt>"` per task and
parses the JSON event stream — summing `input_tokens`/`output_tokens` from
`turn_end` events and counting `tool_call` events — then times the whole task.

### Run a comparison contestant

```sh
cargo run --release -p origin-bench -- run-subprocess \
  --name claude-code --bin /path/to/cc --tasks bench/tasks > cc.json
```

### Render a report

```sh
# Accepts a single results file OR a directory of result JSONs.
cargo run --release -p origin-bench -- report --results . --out bench-report.md
```

The Markdown report is a per-task table:
`| contestant | task | in | out | ms | tools | pass |`. A JSON renderer is also
available for machine consumption / dashboards.

> `origin run` needs a provider key (`ANTHROPIC_API_KEY` or your configured
> provider) to execute real turns; without one the agent steps fail and tasks
> won't pass.

---

## Performance KPIs

The product perf targets the bench and live signals track:

| KPI | Target | Where measured |
|---|---|---|
| **Cold start to first prompt** | < 50 ms (gate proxy: read-only `wall_ms` ≤ 80 ms) | `perf-gate.yml` over read-only tasks. |
| **Keystroke / turn latency** | within each task's `max_turn_latency_ms` | `wall_ms` per task; trace span `dur_us`. |
| **RSS** | under the supervisor soft budget (default 1 GiB) | supervisor lifecycle; `ORIGIN_SUPERVISOR_MEM_BUDGET_MB`. |
| **Cache hit rate** | ≥ 70% | token planner → `origin_cache_hit_total` + traces (not asserted by the gate). |

Token budgets and tool-call ceilings per task guard against an agent that "passes"
by burning context or thrashing tools.

---

## The CI perf gate

`perf-gate.yml` runs on every PR to `dev`/`main` (and on dispatch). It builds the
release binaries, runs the harness over `bench/tasks`, and asserts the read-only
gate:

```sh
cargo build --release --locked -p origin-cli -p origin-daemon
cargo run --release --locked -p origin-bench -- run-origin --tasks bench/tasks > result.json
# Gate: WORST wall_ms across tasks 01-/02- must be <= 80 ms, else fail.
```

The 80 ms read-only ceiling is the GA-acceptance proxy for the < 50 ms cold-start
target; the ≥ 70% cache-hit-rate KPI is surfaced in traces/metrics but is **not**
asserted in the gate.

### When the gate is red

1. Reproduce locally with the two commands above.
2. Inspect `result.json` — find the worst-`wall_ms` read-only task.
3. Profile **cold start / agent overhead**, not just model time: the read-only
   tasks are short, so a regression is usually startup, IPC, or planning cost.
4. Cross-check the parquet trace ring for slow spans by `tool`/`kind`
   (see [`observability-runbook.md`](./observability-runbook.md)).

---

## Quick reference

```sh
# List tasks
cargo run -p origin-bench -- list

# Benchmark origin (release)
cargo build --release -p origin-cli -p origin-daemon
ORIGIN_BIN=target/release/origin \
  cargo run --release -p origin-bench -- run-origin --tasks bench/tasks > result.json

# Compare a contestant
cargo run --release -p origin-bench -- run-subprocess --name X --bin /path/X --tasks bench/tasks > X.json

# Report
cargo run --release -p origin-bench -- report --results . --out report.md
```

| Env / arg | Effect |
|---|---|
| `ORIGIN_BIN` | Path to the `origin` binary the runner drives (default `target/debug/origin`). |
| `--tasks <dir>` | Task-set root containing `manifest.json`. |
| `--name` / `--bin` | Name + binary of a comparison contestant (`run-subprocess`). |
| `--results` / `--out` | Input result file/dir and output Markdown path (`report`). |

---

_Last reviewed against workspace version 0.9.8._
