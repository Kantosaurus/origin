// SPDX-License-Identifier: Apache-2.0
//! `origin bench` — drive the [`origin_bench`] task set and emit the
//! multi-sample reliability report (pass@k / pass^k / flakiness + a failure
//! histogram).
//!
//! The reliability engine lives in [`origin_bench::reliability`]; this module
//! is the user-facing surface that makes it reachable from the `origin` binary.
//! Two paths are supported so the engine is usable whether or not a live
//! provider/daemon is available:
//!
//! * **live** (default): run the task set through the offline-capable
//!   [`origin_bench::runner_subprocess`], which shells a per-task command
//!   against a contestant binary (no live LLM is wired into the harness
//!   itself — the binary you point it at decides what happens). Each task is
//!   sampled `samples` times to build [`origin_bench::reliability::TaskSamples`].
//!   Because [`origin_bench::metrics::TaskResult`] carries no captured output,
//!   this path produces pass/fail outcomes (driving pass@k / pass^k /
//!   flakiness) with an empty failure histogram.
//! * **`--from <results.json>`**: read a recorded `Vec<TaskResult>` (the exact
//!   shape [`origin_bench::report::render_json`] emits), group repeated
//!   `(contestant, task_id)` rows into samples, then compute and render the
//!   report. This needs neither a provider nor a daemon, so the engine is
//!   reachable purely offline and is the path exercised by the unit tests.
//!
//! `k` for the pass@k / pass^k columns is taken to be `samples` (the number of
//! independent runs collected per task), matching the harness's intent that
//! "k = the budget of attempts you would actually make".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use origin_bench::metrics::TaskResult;
use origin_bench::reliability::{render_json, render_markdown, ReliabilityReport, TaskSamples};

/// Hard cap on `--samples` to bound work (and, on the live path, subprocess
/// spawns). A reliability run with more than this many samples is almost
/// certainly a typo; reject it rather than fork-bomb the machine.
const MAX_SAMPLES: u32 = 1024;

/// Hard cap on the size of a `--from` results file we will read into memory.
/// 64 MiB is far more than any realistic recorded-results JSON and stops a
/// pathological/untrusted path from exhausting memory.
const MAX_FROM_BYTES: u64 = 64 * 1024 * 1024;

/// Run `origin bench`.
///
/// With `from == Some(path)` the recorded results at `path` are grouped into
/// per-task samples and rendered (offline, no provider needed). Otherwise the
/// task set is run live `samples` times through the subprocess runner. Output
/// is Markdown unless `json` is set.
///
/// # Errors
/// Returns an error if `samples` is out of range, the `--from` file cannot be
/// read/parsed or is too large, the live task set cannot be loaded, or a live
/// task run fails to spawn.
pub fn run(samples: u32, json: bool, from: Option<String>) -> Result<()> {
    if samples == 0 || samples > MAX_SAMPLES {
        anyhow::bail!("--samples must be in 1..={MAX_SAMPLES} (got {samples})");
    }

    let report = from.map_or_else(
        || report_from_live(samples),
        |path| report_from_recorded(&path, samples),
    )?;

    let rendered = if json {
        render_json(&report)
    } else {
        render_markdown(&report)
    };
    println!("{rendered}");
    Ok(())
}

/// Build a [`ReliabilityReport`] from a recorded `Vec<TaskResult>` JSON file.
///
/// Repeated `(contestant, task_id)` rows are treated as independent samples of
/// the same task. `k` is `samples`, clamped per task to the number of samples
/// actually present (the estimators in [`origin_bench::reliability`] already
/// clamp `k` to `n`, so the caller's `samples` is just an upper bound here).
fn report_from_recorded(path: &str, samples: u32) -> Result<ReliabilityReport> {
    let results = read_recorded_results(path)?;
    let task_samples = group_into_samples(&results);
    Ok(ReliabilityReport::build(&task_samples, samples))
}

/// Read + validate a `--from` results file into a `Vec<TaskResult>`.
///
/// The path is checked to be an existing regular file under the size cap before
/// the bytes are parsed, so an untrusted/oversized path is rejected up front
/// rather than read blindly.
fn read_recorded_results(path: &str) -> Result<Vec<TaskResult>> {
    let p = Path::new(path);
    let meta = std::fs::metadata(p).with_context(|| format!("reading --from file `{path}`"))?;
    if !meta.is_file() {
        anyhow::bail!("--from `{path}` is not a regular file");
    }
    if meta.len() > MAX_FROM_BYTES {
        anyhow::bail!(
            "--from `{path}` is {} bytes, exceeding the {MAX_FROM_BYTES}-byte cap",
            meta.len()
        );
    }
    let body = std::fs::read(p).with_context(|| format!("reading --from file `{path}`"))?;
    serde_json::from_slice(&body).with_context(|| format!("parsing --from file `{path}` as a TaskResult array"))
}

/// Group a flat list of single-run results into per-task multi-sample sets.
///
/// Each distinct `(contestant, task_id)` becomes one [`TaskSamples`]; every row
/// for that key contributes one pass/fail outcome, in input order. Insertion
/// order is preserved so the rendered report is stable for a given input.
fn group_into_samples(results: &[TaskResult]) -> Vec<TaskSamples> {
    // Map key -> index into `out`, so we keep first-seen ordering while still
    // grouping repeated rows in O(n log n).
    let mut index: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut out: Vec<TaskSamples> = Vec::new();
    for r in results {
        let key = (r.contestant.clone(), r.task_id.clone());
        if let Some(&i) = index.get(&key) {
            out[i].outcomes.push(r.passed);
        } else {
            index.insert(key, out.len());
            out.push(TaskSamples::new(
                r.contestant.clone(),
                r.task_id.clone(),
                vec![r.passed],
            ));
        }
    }
    out
}

/// Build a [`ReliabilityReport`] by running the task set live, `samples` times
/// per task, through the offline-capable subprocess runner.
///
/// The contestant binary is `$ORIGIN_BENCH_BIN` (falling back to `$ORIGIN_BIN`,
/// then `origin`); the task set is `$ORIGIN_BENCH_TASKS` (falling back to
/// `bench/tasks` relative to the current directory). `k` is `samples`.
fn report_from_live(samples: u32) -> Result<ReliabilityReport> {
    let tasks_root = tasks_root();
    let tasks = origin_bench::task_set::load(&tasks_root)
        .with_context(|| format!("loading bench task set from `{}`", tasks_root.display()))?;

    let bin = contestant_bin();
    let cap = usize::try_from(samples).unwrap_or(0);
    let mut samples_by_task: Vec<TaskSamples> = tasks
        .iter()
        .map(|t| TaskSamples::new("origin", t.id.clone(), Vec::with_capacity(cap)))
        .collect();

    for _ in 0..samples {
        for (slot, task) in samples_by_task.iter_mut().zip(tasks.iter()) {
            let result = origin_bench::runner_subprocess::run_one("origin", &bin, &[], task)
                .with_context(|| format!("running bench task `{}`", task.id))?;
            slot.outcomes.push(result.passed);
        }
    }

    Ok(ReliabilityReport::build(&samples_by_task, samples))
}

/// Resolve the contestant binary for the live path.
fn contestant_bin() -> PathBuf {
    std::env::var_os("ORIGIN_BENCH_BIN")
        .or_else(|| std::env::var_os("ORIGIN_BIN"))
        .map_or_else(|| PathBuf::from("origin"), PathBuf::from)
}

/// Resolve the task-set root for the live path.
fn tasks_root() -> PathBuf {
    std::env::var_os("ORIGIN_BENCH_TASKS")
        .map_or_else(|| PathBuf::from("bench/tasks"), PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(contestant: &str, task: &str, passed: bool) -> TaskResult {
        TaskResult {
            contestant: contestant.to_string(),
            task_id: task.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            wall_ms: 0,
            tool_calls: 0,
            passed,
        }
    }

    #[test]
    fn group_into_samples_collapses_repeated_keys() {
        let rows = vec![
            result("origin", "t1", true),
            result("origin", "t1", false),
            result("origin", "t1", true),
            result("origin", "t2", true),
        ];
        let grouped = group_into_samples(&rows);
        assert_eq!(grouped.len(), 2);
        // First-seen order is preserved: t1 before t2.
        assert_eq!(grouped[0].task_id, "t1");
        assert_eq!(grouped[0].outcomes, vec![true, false, true]);
        assert_eq!(grouped[1].task_id, "t2");
        assert_eq!(grouped[1].outcomes, vec![true]);
    }

    #[test]
    fn recorded_report_markdown_has_expected_lines() {
        // t1: 2/3 pass; t2: always passes.
        let rows = vec![
            result("origin", "t1", true),
            result("origin", "t1", true),
            result("origin", "t1", false),
            result("origin", "t2", true),
            result("origin", "t2", true),
            result("origin", "t2", true),
        ];
        let grouped = group_into_samples(&rows);
        let report = ReliabilityReport::build(&grouped, 3);
        let md = render_markdown(&report);
        assert!(md.contains("# Origin reliability report"));
        assert!(md.contains("pass@3"));
        assert!(md.contains("pass^3"));
        assert!(md.contains("mean pass@3"));
        assert!(md.contains("## Failure patterns"));
        // Both task rows are present.
        assert!(md.contains("| origin | t1 |"));
        assert!(md.contains("| origin | t2 |"));
    }

    #[test]
    fn recorded_report_json_round_trips_a_tiny_results_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("results.json");
        let rows = vec![
            result("origin", "t1", true),
            result("origin", "t1", false),
        ];
        let body = serde_json::to_string(&rows).expect("serialize results");
        std::fs::write(&path, body).expect("write results");

        let report =
            report_from_recorded(path.to_str().expect("utf8 path"), 2).expect("build report");
        assert_eq!(report.k, 2);
        assert_eq!(report.tasks.len(), 1);
        assert_eq!(report.tasks[0].samples, 2);
        assert_eq!(report.tasks[0].passes, 1);

        let json = render_json(&report);
        assert!(json.contains("\"k\": 2"));
        assert!(json.contains("failure_histogram"));
    }

    #[test]
    fn read_recorded_results_rejects_missing_file() {
        let err = read_recorded_results("definitely-not-a-real-file.json").expect_err("should fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("definitely-not-a-real-file.json"), "got: {msg}");
    }

    #[test]
    fn run_rejects_out_of_range_samples() {
        assert!(run(0, false, None).is_err());
        assert!(run(MAX_SAMPLES + 1, false, None).is_err());
    }
}
