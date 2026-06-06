// SPDX-License-Identifier: Apache-2.0
//! Task-success reliability metrics over repeated ("multi-sample") runs.
//!
//! Where [`crate::metrics::TaskResult`] captures a *single* run of one task,
//! a real reliability picture needs **K independent samples** of the same
//! task so we can reason about variance, not just a point estimate. This
//! module adds:
//!
//! * [`TaskSamples`] — K pass/fail outcomes for one task.
//! * [`pass_at_k`] — the standard unbiased pass@k estimator
//!   `1 - C(n-c, k)/C(n, k)` (probability at least one of `k` sampled runs
//!   passes).
//! * [`pass_caret_k`] — the "pass-power-k" reliability metric
//!   `C(c, k)/C(n, k)` (probability *all* `k` sampled runs pass).
//! * [`flakiness`] — a `0..=1` instability score peaking at a 0.5 pass rate.
//! * [`FailurePattern`] + [`classify_failure`] — cheap substring triage of a
//!   failing run's captured output.
//! * [`ReliabilityReport`] — a task-set aggregate with a failure histogram
//!   plus Markdown/JSON renderers matching [`crate::report`].
//!
//! All combinatorial math uses the *product* form rather than full
//! factorials, so it stays exact-ish and overflow-free for the small `n`/`k`
//! a benchmark realistically produces.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// K independent pass/fail outcomes for a single benchmark task.
///
/// `outcomes[i] == true` means sample `i` passed. The contestant is carried
/// so a report can group samples produced by different CLIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSamples {
    pub contestant: String,
    pub task_id: String,
    /// One entry per independent run; `true` == passed.
    pub outcomes: Vec<bool>,
    /// Captured output for each *failing* run, kept for failure triage.
    ///
    /// Need not be aligned 1:1 with `outcomes`; only failures are recorded.
    #[serde(default)]
    pub failure_logs: Vec<String>,
}

impl TaskSamples {
    /// Build a sample set from raw outcomes (no failure logs).
    #[must_use]
    pub fn new(contestant: impl Into<String>, task_id: impl Into<String>, outcomes: Vec<bool>) -> Self {
        Self {
            contestant: contestant.into(),
            task_id: task_id.into(),
            outcomes,
            failure_logs: Vec::new(),
        }
    }

    /// Total number of samples `n`.
    #[must_use]
    pub fn n(&self) -> u32 {
        u32::try_from(self.outcomes.len()).unwrap_or(u32::MAX)
    }

    /// Number of passing samples `c`.
    #[must_use]
    pub fn c(&self) -> u32 {
        u32::try_from(self.outcomes.iter().filter(|&&p| p).count()).unwrap_or(u32::MAX)
    }

    /// Fraction of samples that passed, in `0..=1` (`0.0` when there are none).
    #[must_use]
    pub fn pass_rate(&self) -> f64 {
        let n = self.n();
        if n == 0 {
            return 0.0;
        }
        f64::from(self.c()) / f64::from(n)
    }

    /// pass@k for this sample set; see [`pass_at_k`].
    #[must_use]
    pub fn pass_at_k(&self, k: u32) -> f64 {
        pass_at_k(self.n(), self.c(), k)
    }

    /// pass^k for this sample set; see [`pass_caret_k`].
    #[must_use]
    pub fn pass_caret_k(&self, k: u32) -> f64 {
        pass_caret_k(self.n(), self.c(), k)
    }

    /// Flakiness for this sample set; see [`flakiness`].
    #[must_use]
    pub fn flakiness(&self) -> f64 {
        flakiness(self.pass_rate())
    }
}

/// Unbiased **pass@k** estimator: the probability that at least one of `k`
/// runs drawn (without replacement) from `n` samples passes, given `c` of the
/// `n` samples passed.
///
/// Closed form `1 - C(n-c, k)/C(n, k)`, evaluated as the product
/// `1 - prod_{i=0}^{k-1} (n-c-i)/(n-i)` to dodge factorial overflow.
///
/// Edge cases: returns `0.0` when `n == 0`; clamps `k` to `n` (sampling more
/// than exist is treated as sampling all of them); if there are fewer than `k`
/// *failing* samples then every draw must include a pass, so the result is
/// `1.0`.
#[must_use]
pub fn pass_at_k(n: u32, c: u32, k: u32) -> f64 {
    if n == 0 || k == 0 {
        return 0.0;
    }
    let k = k.min(n);
    let fails = n - c.min(n);
    // Not enough failing samples to fill a k-draw with only failures.
    if fails < k {
        return 1.0;
    }
    // prod_{i=0}^{k-1} (fails - i) / (n - i)  == C(fails, k) / C(n, k)
    let mut prob_all_fail = 1.0_f64;
    for i in 0..k {
        let num = f64::from(fails - i);
        let den = f64::from(n - i);
        prob_all_fail *= num / den;
    }
    (1.0 - prob_all_fail).clamp(0.0, 1.0)
}

/// **pass^k** ("pass-power-k") reliability metric: the probability that *all*
/// `k` runs drawn (without replacement) from `n` samples pass.
///
/// `c >= k  ⇒  C(c, k)/C(n, k)`, otherwise `0.0` (cannot draw `k` passes from
/// fewer than `k` passing samples). Evaluated as the product
/// `prod_{i=0}^{k-1} (c-i)/(n-i)` for overflow safety.
///
/// Edge cases: returns `0.0` when `n == 0`; `k == 0` is the empty draw and
/// vacuously succeeds (`1.0`); `k` is clamped to `n`.
#[must_use]
pub fn pass_caret_k(n: u32, c: u32, k: u32) -> f64 {
    if k == 0 {
        return 1.0;
    }
    if n == 0 {
        return 0.0;
    }
    let k = k.min(n);
    let c = c.min(n);
    if c < k {
        return 0.0;
    }
    let mut prob_all_pass = 1.0_f64;
    for i in 0..k {
        let num = f64::from(c - i);
        let den = f64::from(n - i);
        prob_all_pass *= num / den;
    }
    prob_all_pass.clamp(0.0, 1.0)
}

/// **Flakiness** score in `0..=1` for a task given its pass rate.
///
/// `1 - |2 * pass_rate - 1|`: `0.0` for a task that always passes or always
/// fails (deterministic), peaking at `1.0` for a 50/50 task (maximally
/// unreliable). The input is clamped to `0..=1` defensively.
#[must_use]
pub fn flakiness(pass_rate: f64) -> f64 {
    let p = pass_rate.clamp(0.0, 1.0);
    1.0 - (2.0_f64.mul_add(p, -1.0)).abs()
}

/// Coarse classification of *why* a run failed, derived from its captured
/// output via cheap substring heuristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FailurePattern {
    /// Source failed to compile (e.g. `error[E0277]`, `cannot find`).
    CompileError,
    /// A test assertion / test run reported failures.
    TestFailure,
    /// The run exceeded its time budget.
    Timeout,
    /// A tool or external command could not be invoked.
    ToolError,
    /// Anything not matched by the heuristics above.
    Other,
}

impl FailurePattern {
    /// Stable lowercase label used in histograms and serialized reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::CompileError => "compile_error",
            Self::TestFailure => "test_failure",
            Self::Timeout => "timeout",
            Self::ToolError => "tool_error",
            Self::Other => "other",
        }
    }
}

/// Classify a failing run's captured output into a [`FailurePattern`].
///
/// Order matters: timeouts and missing-tool errors are checked before the
/// generic compile/test heuristics so that, e.g., a "command not found" line
/// is not misread as a compile error. The match is case-insensitive on a
/// single lowercased copy of the input.
#[must_use]
pub fn classify_failure(output: &str) -> FailurePattern {
    let lower = output.to_ascii_lowercase();
    let has = |needle: &str| lower.contains(needle);

    if has("timed out") || has("timeout") || has("deadline exceeded") {
        return FailurePattern::Timeout;
    }
    if has("command not found")
        || has("no such file or directory")
        || has("permission denied")
        || has("tool error")
        || has("executablenotfound")
    {
        return FailurePattern::ToolError;
    }
    // `error[E0599]` etc. — rustc diagnostics carry a bracketed code.
    if has("error[e") || has("could not compile") || has("cannot find") || has("mismatched types") {
        return FailurePattern::CompileError;
    }
    if has("test result: failed")
        || has("panicked")
        || has("assertion")
        || has("assert_eq")
        || has("failures:")
    {
        return FailurePattern::TestFailure;
    }
    FailurePattern::Other
}

/// Per-task row in a [`ReliabilityReport`].
#[allow(clippy::module_name_repetitions)] // `TaskReliability` is unambiguous from outside the crate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskReliability {
    pub contestant: String,
    pub task_id: String,
    pub samples: u32,
    pub passes: u32,
    pub pass_rate: f64,
    pub pass_at_1: f64,
    pub pass_at_k: f64,
    pub pass_caret_k: f64,
    pub flakiness: f64,
}

/// Aggregate reliability over a whole task set, plus a failure histogram.
#[allow(clippy::module_name_repetitions)] // `ReliabilityReport` is the documented public report type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReliabilityReport {
    /// The `k` used for the `pass_at_k` / `pass_caret_k` columns.
    pub k: u32,
    pub tasks: Vec<TaskReliability>,
    /// Count of classified failures across every sample in the set.
    pub failure_histogram: BTreeMap<String, u64>,
}

impl ReliabilityReport {
    /// Build a report from a slice of multi-sample task results at a given `k`.
    ///
    /// Each task contributes one [`TaskReliability`] row; every recorded
    /// failure log is classified and tallied into [`Self::failure_histogram`].
    #[must_use]
    pub fn build(samples: &[TaskSamples], k: u32) -> Self {
        let mut tasks = Vec::with_capacity(samples.len());
        let mut histogram: BTreeMap<String, u64> = BTreeMap::new();

        for ts in samples {
            tasks.push(TaskReliability {
                contestant: ts.contestant.clone(),
                task_id: ts.task_id.clone(),
                samples: ts.n(),
                passes: ts.c(),
                pass_rate: ts.pass_rate(),
                pass_at_1: ts.pass_at_k(1),
                pass_at_k: ts.pass_at_k(k),
                pass_caret_k: ts.pass_caret_k(k),
                flakiness: ts.flakiness(),
            });
            for log in &ts.failure_logs {
                let label = classify_failure(log).label().to_owned();
                *histogram.entry(label).or_insert(0) += 1;
            }
        }

        Self {
            k,
            tasks,
            failure_histogram: histogram,
        }
    }

    /// Mean pass@k across all tasks (`0.0` for an empty report).
    #[must_use]
    pub fn mean_pass_at_k(&self) -> f64 {
        if self.tasks.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.tasks.iter().map(|t| t.pass_at_k).sum();
        sum / f64::from(u32::try_from(self.tasks.len()).unwrap_or(u32::MAX))
    }

    /// Mean flakiness across all tasks (`0.0` for an empty report).
    #[must_use]
    pub fn mean_flakiness(&self) -> f64 {
        if self.tasks.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.tasks.iter().map(|t| t.flakiness).sum();
        sum / f64::from(u32::try_from(self.tasks.len()).unwrap_or(u32::MAX))
    }
}

/// Render a [`ReliabilityReport`] as Markdown, mirroring [`crate::report`].
#[must_use]
pub fn render_markdown(report: &ReliabilityReport) -> String {
    let mut s = String::new();
    writeln!(s, "# Origin reliability report").ok();
    writeln!(s).ok();
    writeln!(s, "k = {}", report.k).ok();
    writeln!(s).ok();
    writeln!(
        s,
        "| contestant | task | n | c | pass_rate | pass@1 | pass@{k} | pass^{k} | flakiness |",
        k = report.k,
    )
    .ok();
    writeln!(s, "|---|---|---:|---:|---:|---:|---:|---:|---:|").ok();
    for t in &report.tasks {
        writeln!(
            s,
            "| {} | {} | {} | {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} |",
            t.contestant,
            t.task_id,
            t.samples,
            t.passes,
            t.pass_rate,
            t.pass_at_1,
            t.pass_at_k,
            t.pass_caret_k,
            t.flakiness,
        )
        .ok();
    }
    writeln!(s).ok();
    writeln!(s, "mean pass@{} = {:.3}", report.k, report.mean_pass_at_k()).ok();
    writeln!(s, "mean flakiness = {:.3}", report.mean_flakiness()).ok();
    writeln!(s).ok();
    writeln!(s, "## Failure patterns").ok();
    writeln!(s).ok();
    writeln!(s, "| pattern | count |").ok();
    writeln!(s, "|---|---:|").ok();
    for (pattern, count) in &report.failure_histogram {
        writeln!(s, "| {pattern} | {count} |").ok();
    }
    s
}

/// Render a [`ReliabilityReport`] as pretty JSON, mirroring [`crate::report`].
#[must_use]
pub fn render_json(report: &ReliabilityReport) -> String {
    serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {b}, got {a}");
    }

    #[test]
    fn pass_at_k_known_values() {
        // n=5, c=3, k=1  =>  1 - C(2,1)/C(5,1) = 1 - 2/5 = 0.6
        approx(pass_at_k(5, 3, 1), 0.6);
        // n=5, c=2, k=2  =>  1 - C(3,2)/C(5,2) = 1 - 3/10 = 0.7
        approx(pass_at_k(5, 2, 2), 0.7);
        // pass@1 is exactly the pass rate.
        approx(pass_at_k(4, 1, 1), 0.25);
    }

    #[test]
    fn pass_at_k_edges() {
        approx(pass_at_k(0, 0, 1), 0.0); // no samples
        approx(pass_at_k(5, 0, 1), 0.0); // never passes
        approx(pass_at_k(5, 5, 1), 1.0); // always passes
        approx(pass_at_k(5, 3, 3), 1.0); // only 2 fails, can't fill a 3-draw
        approx(pass_at_k(5, 3, 9), 1.0); // k clamped to n; <k fails => 1.0
    }

    #[test]
    fn pass_caret_k_known_values() {
        // n=5, c=2, k=2  =>  C(2,2)/C(5,2) = 1/10 = 0.1
        approx(pass_caret_k(5, 2, 2), 0.1);
        // n=5, c=3, k=1  =>  C(3,1)/C(5,1) = 3/5 = 0.6
        approx(pass_caret_k(5, 3, 1), 0.6);
        // c < k  =>  0.0
        approx(pass_caret_k(5, 1, 2), 0.0);
        // all pass, any k <= n => 1.0
        approx(pass_caret_k(5, 5, 3), 1.0);
        // empty draw is vacuously all-pass.
        approx(pass_caret_k(5, 0, 0), 1.0);
    }

    #[test]
    fn flakiness_known_values() {
        approx(flakiness(1.0), 0.0); // always pass
        approx(flakiness(0.0), 0.0); // always fail
        approx(flakiness(0.5), 1.0); // maximally flaky
        approx(flakiness(0.25), 0.5);
        approx(flakiness(0.75), 0.5);
    }

    #[test]
    fn classifier_representative_strings() {
        assert_eq!(
            classify_failure("error[E0277]: the trait bound is not satisfied"),
            FailurePattern::CompileError
        );
        assert_eq!(
            classify_failure("could not compile `foo` due to 2 previous errors"),
            FailurePattern::CompileError
        );
        assert_eq!(
            classify_failure("test result: FAILED. 1 passed; 1 failed"),
            FailurePattern::TestFailure
        );
        assert_eq!(
            classify_failure("thread 'main' panicked at src/lib.rs:3:5"),
            FailurePattern::TestFailure
        );
        assert_eq!(
            classify_failure("the run timed out after 60s"),
            FailurePattern::Timeout
        );
        assert_eq!(
            classify_failure("bash: cargoo: command not found"),
            FailurePattern::ToolError
        );
        assert_eq!(
            classify_failure("everything was fine, just slow"),
            FailurePattern::Other
        );
    }

    #[test]
    fn classifier_priority_timeout_over_compile() {
        // A timeout line plus a stray compile-ish token must stay a Timeout.
        assert_eq!(
            classify_failure("error[E] build still running... deadline exceeded"),
            FailurePattern::Timeout
        );
    }

    #[test]
    fn task_samples_aggregates() {
        let ts = TaskSamples::new("origin", "t1", vec![true, false, true, true, false]);
        assert_eq!(ts.n(), 5);
        assert_eq!(ts.c(), 3);
        approx(ts.pass_rate(), 0.6);
        approx(ts.pass_at_k(1), 0.6);
    }

    #[test]
    fn report_build_and_render() {
        let mut a = TaskSamples::new("origin", "t1", vec![true, true, false, false, true]);
        a.failure_logs = vec![
            "error[E0599] no method named foo".to_string(),
            "test result: FAILED".to_string(),
        ];
        let b = TaskSamples::new("origin", "t2", vec![true, true, true, true, true]);
        let report = ReliabilityReport::build(&[a, b], 2);

        assert_eq!(report.tasks.len(), 2);
        assert_eq!(report.k, 2);
        assert_eq!(report.failure_histogram.get("compile_error"), Some(&1));
        assert_eq!(report.failure_histogram.get("test_failure"), Some(&1));

        // t2 always passes => flakiness 0, pass@2 = 1.
        let t2 = &report.tasks[1];
        approx(t2.flakiness, 0.0);
        approx(t2.pass_at_k, 1.0);
        approx(t2.pass_caret_k, 1.0);

        let md = render_markdown(&report);
        assert!(md.contains("# Origin reliability report"));
        assert!(md.contains("pass@2"));
        assert!(md.contains("compile_error"));

        let json = render_json(&report);
        assert!(json.contains("\"k\": 2"));
        assert!(json.contains("failure_histogram"));
    }
}
