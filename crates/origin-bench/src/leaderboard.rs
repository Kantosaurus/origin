// SPDX-License-Identifier: Apache-2.0
//! Cross-contestant **leaderboard**: aggregate many models'/contestants'
//! recorded bench results into a single ranked table (Markdown / JSON).
//!
//! Where [`crate::reliability`] reports per-task reliability for one run, this
//! module collapses a flat `Vec<`[`TaskResult`]`>` (which may mix several
//! contestants and repeated `(contestant, task)` rows) into one row per
//! contestant — mean pass@k, mean flakiness and mean wall-clock — then ranks
//! them: highest mean pass@k first, ties broken by lower flakiness, then faster
//! mean latency, then contestant name (so the order is fully deterministic and
//! reproducible). The aggregation + ranking are pure functions, unit-tested
//! without any provider or I/O.
#![allow(clippy::module_name_repetitions)]

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::metrics::TaskResult;
use crate::reliability::{flakiness, pass_at_k};

/// One contestant's aggregated standing on the leaderboard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeaderboardEntry {
    /// The contestant (model / binary) name.
    pub contestant: String,
    /// Number of distinct tasks the contestant has results for.
    pub tasks: u32,
    /// Mean pass@k across the contestant's tasks (higher is better).
    pub mean_pass_at_k: f64,
    /// Mean flakiness across the contestant's tasks (lower is better).
    pub mean_flakiness: f64,
    /// Mean wall-clock per recorded run, in milliseconds (lower is better).
    pub mean_wall_ms: f64,
    /// 1-based position after the deterministic ranking sort.
    pub rank: u32,
}

/// A ranked leaderboard for a given `k`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Leaderboard {
    /// The `k` used for the pass@k column.
    pub k: u32,
    /// Entries in ranked order (best first).
    pub entries: Vec<LeaderboardEntry>,
}

/// Per-(contestant, task) tally accumulated while scanning the rows.
#[derive(Default)]
struct TaskTally {
    passes: u32,
    samples: u32,
}

/// Per-contestant accumulator.
#[derive(Default)]
struct ContestantAcc {
    /// `task_id` -> pass/sample tally (`BTree` for deterministic iteration).
    tasks: BTreeMap<String, TaskTally>,
    wall_sum_ms: u64,
    row_count: u64,
}

/// Mean of `u64` sum over a `u64` count as `f64` (0.0 when the count is 0).
/// The precision loss is acceptable for a display metric.
#[allow(clippy::cast_precision_loss)]
fn mean_u64(sum: u64, count: u64) -> f64 {
    if count == 0 {
        0.0
    } else {
        sum as f64 / count as f64
    }
}

/// Aggregate a flat result list into one [`LeaderboardEntry`] per contestant.
///
/// Repeated `(contestant, task)` rows are treated as independent samples of that
/// task. Per task we compute pass@k (via [`pass_at_k`]) and flakiness (via
/// [`flakiness`] over the task's pass rate); per contestant those are averaged
/// across tasks, and `mean_wall_ms` is the mean over every recorded row. The
/// returned entries carry `rank = 0`; call [`rank_entries`] (or [`build`]) to
/// order and number them.
#[must_use]
pub fn aggregate_by_contestant(results: &[TaskResult], k: u32) -> Vec<LeaderboardEntry> {
    let mut by_contestant: BTreeMap<String, ContestantAcc> = BTreeMap::new();
    for r in results {
        let acc = by_contestant.entry(r.contestant.clone()).or_default();
        let tally = acc.tasks.entry(r.task_id.clone()).or_default();
        tally.samples = tally.samples.saturating_add(1);
        if r.passed {
            tally.passes = tally.passes.saturating_add(1);
        }
        acc.wall_sum_ms = acc.wall_sum_ms.saturating_add(r.wall_ms);
        acc.row_count = acc.row_count.saturating_add(1);
    }

    by_contestant
        .into_iter()
        .map(|(contestant, acc)| {
            let task_count = acc.tasks.len();
            let mut pass_sum = 0.0_f64;
            let mut flake_sum = 0.0_f64;
            for tally in acc.tasks.values() {
                pass_sum += pass_at_k(tally.samples, tally.passes, k);
                let rate = if tally.samples == 0 {
                    0.0
                } else {
                    f64::from(tally.passes) / f64::from(tally.samples)
                };
                flake_sum += flakiness(rate);
            }
            let divisor = if task_count == 0 {
                1.0
            } else {
                // task_count is a small Vec length; lossless for any real corpus.
                #[allow(clippy::cast_precision_loss)]
                {
                    task_count as f64
                }
            };
            LeaderboardEntry {
                contestant,
                tasks: u32::try_from(task_count).unwrap_or(u32::MAX),
                mean_pass_at_k: pass_sum / divisor,
                mean_flakiness: flake_sum / divisor,
                mean_wall_ms: mean_u64(acc.wall_sum_ms, acc.row_count),
                rank: 0,
            }
        })
        .collect()
}

/// Order `entries` best-first and assign each its 1-based `rank`.
///
/// Sort key: mean pass@k descending, then flakiness ascending (less flaky
/// wins), then mean wall-clock ascending (faster wins), then contestant name
/// ascending. The name tiebreak makes the order total and deterministic, so
/// even fully-tied contestants get a stable, reproducible position.
pub fn rank_entries(entries: &mut [LeaderboardEntry]) {
    entries.sort_by(|a, b| {
        b.mean_pass_at_k
            .total_cmp(&a.mean_pass_at_k)
            .then(a.mean_flakiness.total_cmp(&b.mean_flakiness))
            .then(a.mean_wall_ms.total_cmp(&b.mean_wall_ms))
            .then_with(|| a.contestant.cmp(&b.contestant))
    });
    for (i, entry) in entries.iter_mut().enumerate() {
        entry.rank = u32::try_from(i + 1).unwrap_or(u32::MAX);
    }
}

/// Aggregate `results` and return a ranked [`Leaderboard`] for `k`.
#[must_use]
pub fn build(results: &[TaskResult], k: u32) -> Leaderboard {
    let mut entries = aggregate_by_contestant(results, k);
    rank_entries(&mut entries);
    Leaderboard { k, entries }
}

/// Render the leaderboard as a Markdown table.
#[must_use]
pub fn render_markdown(lb: &Leaderboard) -> String {
    let mut s = String::new();
    writeln!(s, "# Origin bench leaderboard").ok();
    writeln!(s).ok();
    writeln!(s, "k = {}", lb.k).ok();
    writeln!(s).ok();
    writeln!(
        s,
        "| rank | contestant | tasks | pass@{} | flakiness | mean_ms |",
        lb.k
    )
    .ok();
    writeln!(s, "|---:|---|---:|---:|---:|---:|").ok();
    for e in &lb.entries {
        writeln!(
            s,
            "| {} | {} | {} | {:.3} | {:.3} | {:.1} |",
            e.rank, e.contestant, e.tasks, e.mean_pass_at_k, e.mean_flakiness, e.mean_wall_ms,
        )
        .ok();
    }
    s
}

/// Render the leaderboard as pretty JSON (never panics).
#[must_use]
pub fn render_json(lb: &Leaderboard) -> String {
    serde_json::to_string_pretty(lb).unwrap_or_else(|_| "{}".into())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::float_cmp)]

    use super::{
        aggregate_by_contestant, build, rank_entries, render_json, render_markdown,
        LeaderboardEntry,
    };
    use crate::metrics::TaskResult;

    fn entry(contestant: &str, pass: f64, flake: f64, wall: f64) -> LeaderboardEntry {
        LeaderboardEntry {
            contestant: contestant.to_string(),
            tasks: 1,
            mean_pass_at_k: pass,
            mean_flakiness: flake,
            mean_wall_ms: wall,
            rank: 0,
        }
    }

    fn row(contestant: &str, task: &str, passed: bool, wall_ms: u64) -> TaskResult {
        TaskResult {
            contestant: contestant.to_string(),
            task_id: task.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            wall_ms,
            tool_calls: 0,
            passed,
        }
    }

    #[test]
    fn rank_orders_by_pass_at_k_then_tiebreaks() {
        // A and B tie on pass@k (0.9); B is less flaky + faster ⇒ B outranks A.
        let mut entries = vec![
            entry("A", 0.9, 0.4, 50.0),
            entry("B", 0.9, 0.1, 20.0),
            entry("C", 0.5, 0.0, 5.0),
        ];
        rank_entries(&mut entries);
        assert_eq!(entries[0].contestant, "B");
        assert_eq!(entries[0].rank, 1);
        assert_eq!(entries[1].contestant, "A");
        assert_eq!(entries[2].contestant, "C");
        assert_eq!(entries[2].rank, 3);
    }

    #[test]
    fn rank_is_deterministic_on_full_tie_by_name() {
        // Identical on every metric: the lexicographically-smaller name sorts
        // first, so the order is stable/reproducible (no map nondeterminism).
        let mut entries = vec![entry("zeta", 0.5, 0.5, 10.0), entry("alpha", 0.5, 0.5, 10.0)];
        rank_entries(&mut entries);
        assert_eq!(entries[0].contestant, "alpha");
        assert_eq!(entries[1].contestant, "zeta");
    }

    #[test]
    fn aggregate_by_contestant_computes_mean_pass_at_k_and_speed() {
        // m1: t1 = (pass, fail) ⇒ pass@1 = 0.5 ; t2 = (pass, pass) ⇒ pass@1 = 1.0
        // ⇒ mean pass@1 = 0.75. Wall: 10,30,20,40 ⇒ mean 25.0 over 4 rows.
        let rows = vec![
            row("m1", "t1", true, 10),
            row("m1", "t1", false, 30),
            row("m1", "t2", true, 20),
            row("m1", "t2", true, 40),
        ];
        let agg = aggregate_by_contestant(&rows, 1);
        assert_eq!(agg.len(), 1);
        let e = &agg[0];
        assert_eq!(e.contestant, "m1");
        assert_eq!(e.tasks, 2);
        assert!((e.mean_pass_at_k - 0.75).abs() < 1e-9, "pass@k={}", e.mean_pass_at_k);
        assert!((e.mean_wall_ms - 25.0).abs() < 1e-9, "wall={}", e.mean_wall_ms);
    }

    #[test]
    fn build_ranks_multiple_contestants_and_renders() {
        let rows = vec![
            row("strong", "t1", true, 10),
            row("strong", "t2", true, 10),
            row("weak", "t1", false, 10),
            row("weak", "t2", false, 10),
        ];
        let lb = build(&rows, 2);
        assert_eq!(lb.entries.len(), 2);
        assert_eq!(lb.entries[0].contestant, "strong");
        assert_eq!(lb.entries[0].rank, 1);
        assert_eq!(lb.entries[1].contestant, "weak");

        let md = render_markdown(&lb);
        assert!(md.contains("# Origin bench leaderboard"));
        assert!(md.contains("pass@2"));
        assert!(md.contains("| 1 | strong |"));

        let json = render_json(&lb);
        assert!(json.contains("\"k\": 2"));
        assert!(json.contains("entries"));
    }

    #[test]
    fn render_json_and_markdown_never_panic_on_empty() {
        let lb = build(&[], 3);
        assert!(lb.entries.is_empty());
        let json = render_json(&lb);
        assert!(json.contains("\"k\": 3"));
        assert!(json.contains("entries"));
        let md = render_markdown(&lb);
        assert!(md.contains("# Origin bench leaderboard"));
        assert!(md.contains("k = 3"));
    }
}
