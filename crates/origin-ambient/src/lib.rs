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

/// Drives an [`OvernightPlan`] forward through a wall-clock window.
///
/// The driver is **pure**: it never reads the clock, performs I/O, or executes
/// tasks. The caller owns the loop and passes `now_ms` in on every query, while
/// the driver tracks the cursor into the plan and accumulates outcomes
/// (tokens spent, branch lines, opened PRs) for the eventual [`MorningReport`].
///
/// Typical loop:
/// ```
/// use origin_ambient::{AmbientTask, OvernightDriver, OvernightPlan};
///
/// let plan = OvernightPlan::new(vec![AmbientTask::Tests, AmbientTask::Docs], 60_000);
/// let mut driver = OvernightDriver::new(plan, 1_000);
/// while let Some(task) = driver.next_due(5_000) {
///     // ... execute `task`, measuring `tokens` and maybe opening a PR ...
///     driver.record(task, 1_000, None);
/// }
/// let report = driver.into_report(20_234);
/// assert_eq!(report.tokens_spent, 2_000);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OvernightDriver {
    /// The plan being driven (task list + wall-clock ceiling).
    plan: OvernightPlan,
    /// Wall-clock instant the session began, in milliseconds.
    start_ms: u64,
    /// Index of the next task in `plan.tasks` to run.
    cursor: usize,
    /// Each completed task paired with the tokens it spent, in run order.
    ran: Vec<(AmbientTask, u64)>,
    /// Running total of tokens spent across recorded tasks.
    tokens: u64,
    /// Identifiers/URLs of pull requests opened by recorded tasks.
    prs: Vec<String>,
}

impl OvernightDriver {
    /// Construct a driver for `plan`, treating `start_ms` as the session start.
    ///
    /// The cursor begins at the first task and no outcomes are recorded yet.
    #[must_use]
    pub const fn new(plan: OvernightPlan, start_ms: u64) -> Self {
        Self {
            plan,
            start_ms,
            cursor: 0,
            ran: Vec::new(),
            tokens: 0,
            prs: Vec::new(),
        }
    }

    /// Whether the wall-clock window has elapsed at `now_ms`.
    ///
    /// `true` once `now_ms - start_ms >= plan.max_wall_ms`. Saturating in
    /// `now_ms`: a `now_ms` before `start_ms` is treated as zero elapsed time.
    #[must_use]
    const fn window_elapsed(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.start_ms) >= self.plan.max_wall_ms
    }

    /// Return the next task to run, or `None` when the driver should stop.
    ///
    /// Yields `Some(task)` only when the window has **not** yet elapsed at
    /// `now_ms` *and* tasks remain at or after the cursor. This is a peek: it
    /// does **not** advance the cursor — call [`record`](Self::record) once the
    /// task has run.
    #[must_use]
    pub fn next_due(&self, now_ms: u64) -> Option<AmbientTask> {
        if self.window_elapsed(now_ms) {
            return None;
        }
        self.plan.tasks.get(self.cursor).copied()
    }

    /// Record a completed task's outcome and advance the cursor.
    ///
    /// Remembers `task` with the `tokens` it spent (so [`into_report`] can render
    /// a per-task line), accumulates `tokens` into the running total, collects
    /// `pr` when `Some`, and moves the cursor to the next task. The cursor
    /// advance saturates so repeated calls past the end of the plan can never
    /// overflow.
    ///
    /// [`into_report`]: Self::into_report
    pub fn record(&mut self, task: AmbientTask, tokens: u64, pr: Option<String>) {
        self.ran.push((task, tokens));
        self.tokens = self.tokens.saturating_add(tokens);
        if let Some(pr) = pr {
            self.prs.push(pr);
        }
        self.cursor = self.cursor.saturating_add(1);
    }

    /// Whether the session is finished at `now_ms`.
    ///
    /// `true` when the wall-clock window has elapsed **or** every task in the
    /// plan has been recorded (the cursor has reached the end of the list).
    #[must_use]
    pub fn is_finished(&self, now_ms: u64) -> bool {
        self.window_elapsed(now_ms) || self.cursor >= self.plan.tasks.len()
    }

    /// Consume the driver into a [`MorningReport`].
    ///
    /// Each recorded task becomes a `ran` line naming the task, its token cost,
    /// and the PR-gated branch it lands on for `day_unix`, e.g.
    /// `"tests — 1000 tokens (branch origin/ambient/tests-20234)"`. The report's
    /// `tokens_spent` is the accumulated total and `prs_opened` is every
    /// collected PR identifier, in run order.
    #[must_use]
    pub fn into_report(self, day_unix: u64) -> MorningReport {
        let ran = self
            .ran
            .iter()
            .map(|&(task, tokens)| {
                format!(
                    "{} — {tokens} tokens (branch {})",
                    task.slug(),
                    branch_name(task, day_unix)
                )
            })
            .collect();
        MorningReport::new(ran, self.tokens, self.prs)
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

    #[test]
    fn driver_yields_task_while_window_open() {
        let plan = OvernightPlan::new(vec![AmbientTask::Tests, AmbientTask::Docs], 10_000);
        let driver = OvernightDriver::new(plan, 1_000);
        // 1_000 + 5_000 = 6_000 elapsed < 10_000 window, tasks remain.
        assert_eq!(driver.next_due(6_000), Some(AmbientTask::Tests));
        // next_due is a peek: cursor did not move, so it still yields the first.
        assert_eq!(driver.next_due(6_000), Some(AmbientTask::Tests));
        assert!(!driver.is_finished(6_000));
    }

    #[test]
    fn driver_yields_none_when_window_elapsed() {
        let plan = OvernightPlan::new(vec![AmbientTask::Tests], 10_000);
        let driver = OvernightDriver::new(plan, 1_000);
        // Exactly at the ceiling (1_000 + 10_000) counts as elapsed.
        assert_eq!(driver.next_due(11_000), None);
        assert_eq!(driver.next_due(50_000), None);
        assert!(driver.is_finished(11_000));
        // Tasks still remain, but the window closed.
        assert!(driver.is_finished(11_000));
    }

    #[test]
    fn driver_yields_none_and_finishes_when_all_recorded() {
        let plan = OvernightPlan::new(vec![AmbientTask::Tests, AmbientTask::Docs], 1_000_000);
        let mut driver = OvernightDriver::new(plan, 0);
        driver.record(AmbientTask::Tests, 100, None);
        driver.record(AmbientTask::Docs, 200, None);
        // Window is wide open, but every task is consumed.
        assert_eq!(driver.next_due(5_000), None);
        assert!(driver.is_finished(5_000));
        // Recording past the end is saturating and stays finished.
        driver.record(AmbientTask::Refactor, 1, None);
        assert!(driver.is_finished(5_000));
    }

    #[test]
    fn driver_into_report_accumulates_ran_tokens_and_prs() {
        let plan = OvernightPlan::new(
            vec![AmbientTask::Tests, AmbientTask::Docs, AmbientTask::Refactor],
            1_000_000,
        );
        let mut driver = OvernightDriver::new(plan, 0);
        driver.record(AmbientTask::Tests, 1_000, Some("pr-1".to_string()));
        driver.record(AmbientTask::Docs, 250, None); // no PR -> skipped
        driver.record(AmbientTask::Refactor, 750, Some("pr-2".to_string()));

        let report = driver.into_report(20_234);

        assert_eq!(report.tokens_spent, 2_000);
        assert_eq!(report.prs_opened, vec!["pr-1", "pr-2"]); // None skipped
        assert_eq!(report.ran.len(), 3);
        assert_eq!(
            report.ran[0],
            "tests — 1000 tokens (branch origin/ambient/tests-20234)"
        );
        assert_eq!(
            report.ran[1],
            "docs — 250 tokens (branch origin/ambient/docs-20234)"
        );
        assert_eq!(
            report.ran[2],
            "refactor — 750 tokens (branch origin/ambient/refactor-20234)"
        );
    }

    #[test]
    fn driver_drives_a_full_loop() {
        let plan = OvernightPlan::new(vec![AmbientTask::Tests, AmbientTask::Docs], 60_000);
        let mut driver = OvernightDriver::new(plan, 1_000);
        let mut count = 0_u32;
        // now_ms stays well within the window for the whole loop.
        while let Some(task) = driver.next_due(5_000) {
            driver.record(task, 1_000, None);
            count += 1;
            assert!(count <= 2, "loop must terminate after the plan's tasks");
        }
        assert_eq!(count, 2);
        assert!(driver.is_finished(5_000));
        assert_eq!(driver.into_report(1).tokens_spent, 2_000);
    }
}
