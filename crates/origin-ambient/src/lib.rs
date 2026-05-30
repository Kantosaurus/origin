// SPDX-License-Identifier: Apache-2.0
//! Policy for resource-aware always-on and overnight autonomous work.
//!
//! `origin`'s ambient mode lets the daemon do proactive work (running tests,
//! small refactors, doc touch-ups, memory gardening) while the user is idle or
//! asleep — the jcode Ambient/OpenClaw + Overnight idea. This crate is the pure
//! *policy* layer: it decides **when** ambient work may run under an adaptive
//! token budget that always reserves headroom for the user, picks the **next**
//! task round-robin, names a PR-gated **branch**, and assembles a **morning
//! report**. It performs no execution, I/O, or async — the daemon owns the loop.
//!
//! ```
//! use origin_ambient::{AmbientTask, BudgetPolicy, next_task, should_schedule};
//!
//! let budget = BudgetPolicy::new(1_000_000, 200_000);
//! assert!(budget.may_run(500_000, 50_000));   // plenty of room
//! assert!(!budget.may_run(800_000, 50_000));  // would dip into reserve
//!
//! let task = next_task(&[AmbientTask::Tests]);
//! assert_ne!(task, AmbientTask::Tests);       // never repeat immediately
//!
//! assert!(should_schedule(180, 1_320, 360));  // 03:00 is inside 22:00–06:00
//! ```

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Adaptive daily token budget that always protects a user reserve.
///
/// Ambient work spends from `total_daily_tokens`, but the policy refuses to let
/// it consume the final `reserve_for_user` tokens so an interactive session is
/// never starved. This is the "resource-aware" guarantee of ambient mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetPolicy {
    /// Total tokens the account is willing to spend in a day.
    pub total_daily_tokens: u64,
    /// Tokens at the top of the budget that ambient work must never touch.
    pub reserve_for_user: u64,
}

impl BudgetPolicy {
    /// Construct a budget policy.
    ///
    /// `reserve_for_user` is clamped to `total_daily_tokens` so the available
    /// headroom can never be computed as a negative quantity.
    #[must_use]
    pub const fn new(total_daily_tokens: u64, reserve_for_user: u64) -> Self {
        let reserve = if reserve_for_user > total_daily_tokens {
            total_daily_tokens
        } else {
            reserve_for_user
        };
        Self {
            total_daily_tokens,
            reserve_for_user: reserve,
        }
    }

    /// Tokens ambient work may still spend today without touching the reserve.
    ///
    /// Saturating: returns 0 once `spent_today` reaches the spendable ceiling.
    #[must_use]
    pub const fn available(&self, spent_today: u64) -> u64 {
        let spendable = self.total_daily_tokens.saturating_sub(self.reserve_for_user);
        spendable.saturating_sub(spent_today)
    }

    /// Whether a task estimated at `est_cost` tokens may run right now.
    ///
    /// Returns `true` only when the estimate fits entirely within the remaining
    /// non-reserved headroom, so ambient work never dips into the user reserve.
    #[must_use]
    pub const fn may_run(&self, spent_today: u64, est_cost: u64) -> bool {
        est_cost <= self.available(spent_today)
    }
}

/// One kind of proactive ambient task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AmbientTask {
    /// Run the test suite and surface regressions.
    Tests,
    /// Apply a small, safe refactor.
    Refactor,
    /// Improve or fill in documentation.
    Docs,
    /// Tend the persistent memory store ("memory gardening").
    MemoryGarden,
}

impl AmbientTask {
    /// Fixed round-robin order of all task kinds.
    const ORDER: [Self; 4] = [Self::Tests, Self::Refactor, Self::Docs, Self::MemoryGarden];

    /// Lowercase slug used in branch names and reports.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Tests => "tests",
            Self::Refactor => "refactor",
            Self::Docs => "docs",
            Self::MemoryGarden => "memory-garden",
        }
    }
}

/// Pick the next ambient task round-robin, avoiding an immediate repeat.
///
/// Given the most recent tasks (newest last), advance one step past the last
/// task in the fixed [`AmbientTask::ORDER`]. When `recent` is empty the cycle
/// starts at the first task. The result is never equal to `recent`'s last entry.
#[must_use]
pub fn next_task(recent: &[AmbientTask]) -> AmbientTask {
    let order = AmbientTask::ORDER;
    recent.last().map_or(order[0], |last| {
        let idx = order.iter().position(|t| t == last).unwrap_or(0);
        order[(idx + 1) % order.len()]
    })
}

/// Build the PR-gated branch name for `task` on day `day_unix`.
///
/// `day_unix` is a day index (e.g. Unix time divided by 86 400) so each night's
/// work lands on its own branch, e.g. `origin/ambient/tests-20234`.
#[must_use]
pub fn branch_name(task: AmbientTask, day_unix: u64) -> String {
    format!("origin/ambient/{}-{day_unix}", task.slug())
}

/// A plan for an overnight autonomous session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OvernightPlan {
    /// Tasks to attempt, in order.
    pub tasks: Vec<AmbientTask>,
    /// Hard wall-clock ceiling for the whole session, in milliseconds.
    pub max_wall_ms: u64,
}

impl OvernightPlan {
    /// Construct an overnight plan.
    #[must_use]
    pub const fn new(tasks: Vec<AmbientTask>, max_wall_ms: u64) -> Self {
        Self { tasks, max_wall_ms }
    }
}

/// The report assembled for the user the morning after an ambient session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MorningReport {
    /// Human-readable lines describing what ran.
    pub ran: Vec<String>,
    /// Total tokens spent during the session.
    pub tokens_spent: u64,
    /// URLs or identifiers of pull requests opened.
    pub prs_opened: Vec<String>,
}

impl MorningReport {
    /// Construct a morning report.
    #[must_use]
    pub const fn new(ran: Vec<String>, tokens_spent: u64, prs_opened: Vec<String>) -> Self {
        Self {
            ran,
            tokens_spent,
            prs_opened,
        }
    }

    /// Render the report as Markdown for display or commit into the repo.
    ///
    /// Always includes the token total and a pull-requests section so the user
    /// can audit exactly what ambient mode did overnight.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = String::from("# Ambient morning report\n\n");

        out.push_str("## Ran\n\n");
        if self.ran.is_empty() {
            out.push_str("_Nothing ran._\n");
        } else {
            for line in &self.ran {
                out.push_str("- ");
                out.push_str(line);
                out.push('\n');
            }
        }

        out.push_str(&format!("\n## Tokens spent\n\n{}\n", self.tokens_spent));

        out.push_str("\n## PRs opened\n\n");
        if self.prs_opened.is_empty() {
            out.push_str("_None._\n");
        } else {
            for pr in &self.prs_opened {
                out.push_str("- ");
                out.push_str(pr);
                out.push('\n');
            }
        }

        out
    }
}

/// Whether `now_min` falls inside the overnight window `[window_start, window_end)`.
///
/// All values are minutes-of-day in `[0, 1440)`. The window may wrap past
/// midnight: if `window_start > window_end` (e.g. 22:00–06:00) the window spans
/// the day boundary. A start equal to the end is treated as a full-day window.
#[must_use]
pub const fn should_schedule(now_min: u32, window_start: u32, window_end: u32) -> bool {
    if window_start == window_end {
        return true;
    }
    if window_start < window_end {
        now_min >= window_start && now_min < window_end
    } else {
        now_min >= window_start || now_min < window_end
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn may_run_never_dips_into_reserve() {
        let b = BudgetPolicy::new(1_000_000, 200_000);
        // Spendable is 800k. Spending 750k leaves 50k of headroom.
        assert!(b.may_run(750_000, 50_000));
        assert!(!b.may_run(750_000, 50_001));
        // Already at the spendable ceiling: nothing more may run.
        assert!(!b.may_run(800_000, 1));
        assert!(b.may_run(800_000, 0));
    }

    #[test]
    fn available_saturates_and_respects_reserve() {
        let b = BudgetPolicy::new(1_000_000, 200_000);
        assert_eq!(b.available(0), 800_000);
        assert_eq!(b.available(800_000), 0);
        // Overspend past the ceiling saturates to zero rather than underflowing.
        assert_eq!(b.available(5_000_000), 0);
    }

    #[test]
    fn reserve_is_clamped_to_total() {
        // Reserve larger than total must not produce a negative spendable amount.
        let b = BudgetPolicy::new(100, 500);
        assert_eq!(b.reserve_for_user, 100);
        assert_eq!(b.available(0), 0);
        assert!(!b.may_run(0, 1));
    }

    #[test]
    fn next_task_avoids_immediate_repeat_and_cycles() {
        assert_eq!(next_task(&[]), AmbientTask::Tests);
        assert_eq!(next_task(&[AmbientTask::Tests]), AmbientTask::Refactor);
        assert_eq!(next_task(&[AmbientTask::Refactor]), AmbientTask::Docs);
        assert_eq!(next_task(&[AmbientTask::Docs]), AmbientTask::MemoryGarden);
        // Wraps around back to the start.
        assert_eq!(next_task(&[AmbientTask::MemoryGarden]), AmbientTask::Tests);
        // Only the last entry matters for the repeat guard.
        let recent = [AmbientTask::Tests, AmbientTask::Docs];
        let picked = next_task(&recent);
        assert_ne!(picked, AmbientTask::Docs);
        assert_eq!(picked, AmbientTask::MemoryGarden);
    }

    #[test]
    fn branch_name_format() {
        assert_eq!(
            branch_name(AmbientTask::Tests, 20_234),
            "origin/ambient/tests-20234"
        );
        assert_eq!(
            branch_name(AmbientTask::MemoryGarden, 1),
            "origin/ambient/memory-garden-1"
        );
    }

    #[test]
    fn morning_report_markdown_contains_tokens_and_prs() {
        let r = MorningReport::new(
            vec!["ran tests on origin-cost".to_string()],
            12_345,
            vec!["https://github.com/Kantosaurus/origin/pull/42".to_string()],
        );
        let md = r.to_markdown();
        assert!(md.contains("12345"), "token total must appear: {md}");
        assert!(md.contains("pull/42"), "PR link must appear: {md}");
        assert!(md.contains("## Tokens spent"));
        assert!(md.contains("## PRs opened"));
    }

    #[test]
    fn morning_report_markdown_handles_empty() {
        let md = MorningReport::default().to_markdown();
        assert!(md.contains("_Nothing ran._"));
        assert!(md.contains("_None._"));
        assert!(md.contains('0'));
    }

    #[test]
    fn overnight_window_wraps_around_midnight() {
        // Window 22:00 (1320) -> 06:00 (360).
        assert!(should_schedule(180, 1_320, 360)); // 03:00 inside
        assert!(should_schedule(1_320, 1_320, 360)); // exactly start
        assert!(should_schedule(1_439, 1_320, 360)); // 23:59 inside
        assert!(!should_schedule(360, 1_320, 360)); // exactly end -> outside
        assert!(!should_schedule(720, 1_320, 360)); // noon outside
    }

    #[test]
    fn daytime_window_does_not_wrap() {
        // Window 09:00 (540) -> 17:00 (1020).
        assert!(should_schedule(600, 540, 1_020));
        assert!(!should_schedule(60, 540, 1_020));
        assert!(!should_schedule(1_020, 540, 1_020)); // end is exclusive
    }

    #[test]
    fn equal_window_bounds_mean_always_on() {
        assert!(should_schedule(0, 0, 0));
        assert!(should_schedule(720, 600, 600));
    }

    #[test]
    fn overnight_plan_round_trips() {
        let plan = OvernightPlan::new(vec![AmbientTask::Tests, AmbientTask::Docs], 3_600_000);
        assert_eq!(plan.tasks.len(), 2);
        assert_eq!(plan.max_wall_ms, 3_600_000);
        let json = serde_json::to_string(&plan).unwrap();
        let back: OvernightPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, back);
    }
}
