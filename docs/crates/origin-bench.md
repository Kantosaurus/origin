# origin-bench

> Benchmark harness comparing origin against other coding-agent CLIs

## Purpose

`origin-bench` drives a fixed task set through either the in-process origin
runner or a generic subprocess runner (Claude Code, jcode, opencode), collects
per-task metrics, and renders comparison reports. Repeated runs feed multi-sample
reliability metrics (pass@k, pass^k, flakiness) and a ranked leaderboard. It is
both a library (`origin_bench`) and a CLI binary (`origin-bench`), and is
`publish = false` — an internal tool, not a published dependency.

## Public API surface

| Item | Module | Summary |
| --- | --- | --- |
| `Task` / `Manifest` / `load(root)` | `task_set` | Load `manifest.json` + referenced task JSONs. |
| `TaskResult` | `metrics` | Per-task `{ contestant, task_id, tokens, wall_ms, tool_calls, passed }`. |
| `runner_origin::run_one(bin, task)` | `runner_origin` | Run origin in-process against one task. |
| `runner_subprocess::run_one(name, bin, args, task)` | `runner_subprocess` | Run a contestant CLI by subprocess. |
| `report::render_markdown` / `render_json(&[TaskResult])` | `report` | Single-run comparison report. |
| `TaskSamples` | `reliability` | Multi-run `{ contestant, task_id, outcomes, failure_logs }`. |
| `pass_at_k(n, c, k)` / `pass_caret_k(n, c, k)` / `flakiness(rate)` | `reliability` | Multi-sample reliability metrics. |
| `FailurePattern` / `classify_failure(output)` | `reliability` | Bucket a failing run's output. |
| `TaskReliability` / `ReliabilityReport` (+ `render_markdown`/`render_json`) | `reliability` | Aggregated reliability report. |
| `Leaderboard` / `LeaderboardEntry` | `leaderboard` | Ranked contestant standings. |
| `aggregate_by_contestant` / `rank_entries` / `build` / `render_markdown` / `render_json` | `leaderboard` | Build + render the leaderboard. |

## Key types

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub contestant: String,
    pub task_id: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub wall_ms: u64,
    pub tool_calls: u32,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub prompt: String,
    #[serde(default)]
    pub expected_tools_min: Vec<String>,
    pub expected_tool_calls_max: u32,
    pub max_turn_latency_ms: u64,
    pub max_input_tokens: u64,
    pub max_output_tokens: u64,
}
```

## How it works

The CLI (`clap`-derived) exposes `list`, `run-origin`, `run-subprocess`, and
`report` subcommands. A run loads the task set, executes each task through a
runner, and emits `TaskResult`s as JSON; `report` renders a comparison.

```
manifest.json + tasks/*.json ─► task_set::load ─► [Task]
   │
   ├─ runner_origin::run_one  (ORIGIN_BIN, default target/debug/origin)
   └─ runner_subprocess::run_one (name, bin, args)
        │
        ▼
   [TaskResult] ─► report::render_{markdown,json}
   [TaskSamples] ─► reliability: pass_at_k / pass_caret_k / flakiness
                                 classify_failure → FailurePattern
                 ─► leaderboard::build → rank_entries → render
```

`reliability` computes `pass@k` (probability at least one of k samples passes)
and `pass^k` (all k pass) from `n` samples with `c` passes, plus a `flakiness`
score from the pass rate, and `classify_failure` buckets a failing run's captured
output into a `FailurePattern`. The `leaderboard` aggregates `TaskResult`s by
contestant at a given `k`, ranks the entries, and renders Markdown or JSON
standings.

## Dependencies & features

- `origin-core`, `origin-replay` — task execution + replay support.
- `clap` (derive) — CLI; `tokio` (`process`) — subprocess runner; `anyhow` —
  binary error handling; `walkdir` — task discovery; `serde`/`serde_json`.
- Dev: `tempfile`. No cargo features. `[[bin]]` `origin-bench` + `[lib]`
  `origin_bench`.

## Used by

`Grep "origin-bench" glob "crates/*/Cargo.toml"`:

- `crates/origin-bench/Cargo.toml` (self)
- `crates/origin-cli/Cargo.toml`

**Note:** `origin-bench` is `publish = false` and is primarily a binary
(internal benchmarking tool), so it is not consumed as a published library
dependency by the rest of the workspace.

## Testing

`tests/` holds `smoke.rs`, `task_set_shape.rs` (manifest/task JSON shape),
`report_render.rs` (Markdown/JSON rendering), and `polyglot_corpus.rs`
(cross-language task corpus). The reliability and leaderboard modules carry the
pass@k / pass^k / flakiness and ranking logic exercised by these suites.

## See also

- [Benchmarking operations](../operations/benchmarking.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
