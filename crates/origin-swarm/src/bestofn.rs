// SPDX-License-Identifier: Apache-2.0
//! Best-of-N with execution-grounded selection (SWE-bench proposal 2.3).
//!
//! Test-time scaling: for a *hard* instance, generate N candidate solutions in
//! isolated worktrees, run each against the test oracle, and keep the one that
//! actually passes. With an execution oracle the selector is **free of judge
//! error** — a candidate is kept only if its tests are GREEN.
//!
//! This module is the **pure decision core**: it never spawns a worker, runs a
//! test, or touches git. The daemon supplies a [`Candidate`] per attempt (its
//! test outcome + a cheap size signal + the diff), and [`select_best`] returns
//! the winning index deterministically. Keeping the policy here makes it
//! std-only and exhaustively unit-testable; the orchestration (worktrees,
//! scoring, applying the winning diff) lives in the daemon.

#![allow(clippy::module_name_repetitions)]

use serde::{Deserialize, Serialize};

/// The execution-oracle outcome for one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestOutcome {
    /// The test command exited 0 — the candidate is verified-correct.
    Passed,
    /// The test command exited non-zero — at least one test failed.
    Failed,
    /// The oracle could not be run (spawn error / timeout / no command). Treated
    /// as *worse than* a real result: an unverifiable candidate never beats a
    /// verified one, but ranks above nothing.
    Unknown,
}

impl TestOutcome {
    /// Rank for tie-agnostic comparison: `Passed` > `Failed` > `Unknown`.
    const fn rank(self) -> u8 {
        match self {
            Self::Passed => 2,
            Self::Failed => 1,
            Self::Unknown => 0,
        }
    }

    /// Did the oracle confirm this candidate correct?
    #[must_use]
    pub const fn is_passed(self) -> bool {
        matches!(self, Self::Passed)
    }
}

/// One candidate solution produced by a best-of-N attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    /// Stable index of the attempt (0-based), used as the final tie-break so the
    /// selection is deterministic and reproducible.
    pub index: usize,
    /// The test-oracle verdict for this candidate.
    pub outcome: TestOutcome,
    /// Diff size in bytes (a cheap Occam's-razor tie-break: prefer the smallest
    /// change that passes — larger diffs are likelier to carry incidental
    /// breakage the hidden `PASS_TO_PASS` tests would catch).
    pub diff_len: usize,
    /// Whether this candidate produced any change at all. A no-op candidate is
    /// never selected over one that actually edited the tree (an empty diff can
    /// "pass" simply because it changed nothing).
    pub non_empty: bool,
}

/// Pick the winning candidate index, or `None` when there is nothing to select.
///
/// Ordering (best first):
/// 1. **Non-empty** candidates beat empty ones (an empty diff that "passes" only
///    passed because it changed nothing).
/// 2. **Test outcome** rank: `Passed` > `Failed` > `Unknown`.
/// 3. **Smaller diff** wins (Occam's razor — fewer incidental regressions).
/// 4. **Lower index** wins (stable, reproducible tie-break).
///
/// Returns `None` only for an empty slice. Note a winner may still be `Failed`
/// (all candidates failed) — the caller decides whether to apply a best-effort
/// loser or fall back; [`has_verified_winner`] distinguishes the two.
#[must_use]
pub fn select_best(candidates: &[Candidate]) -> Option<usize> {
    candidates
        .iter()
        .max_by(|a, b| {
            a.non_empty
                .cmp(&b.non_empty)
                .then_with(|| a.outcome.rank().cmp(&b.outcome.rank()))
                // Smaller diff is better ⇒ reverse the byte-length comparison.
                .then_with(|| b.diff_len.cmp(&a.diff_len))
                // Lower index is better ⇒ reverse again.
                .then_with(|| b.index.cmp(&a.index))
        })
        .map(|c| c.index)
}

/// Did at least one candidate pass the oracle *and* make a real change?
///
/// The daemon uses this to decide whether best-of-N actually *succeeded*
/// (apply the verified winner) versus merely produced a least-bad loser (fall
/// back to the single-shot result / surface the failures).
#[must_use]
pub fn has_verified_winner(candidates: &[Candidate]) -> bool {
    candidates.iter().any(|c| c.outcome.is_passed() && c.non_empty)
}

/// Signals that mark a single-shot run as "hard" — i.e. worth spending N× on.
///
/// Best-of-N multiplies cost, so it must be **gated** to the residual of
/// instances the cheap single-shot path fails on, never applied uniformly.
/// These are exactly the difficulty signals the agent loop already computes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // a signal record — named difficulty bools read clearer than a bitset
pub struct DifficultySignals {
    /// The single-shot run ended with the test oracle still RED.
    pub tests_red: bool,
    /// The stuck-loop detector fired (repeated no-progress actions).
    pub stuck: bool,
    /// The model reported the goal blocked/unmet, or the verifier rejected it.
    pub unmet: bool,
    /// The run consumed most of its turn budget without settling (a proxy for
    /// flailing). The daemon passes the fraction as a bool over a threshold.
    pub near_budget: bool,
    /// The run touched no files at all (nothing to select among) — a hard
    /// *veto*: best-of-N needs candidate diffs to score, so this forces `false`.
    pub no_edits: bool,
}

impl DifficultySignals {
    /// Should best-of-N fire for this instance?
    ///
    /// True when any positive difficulty signal is set — *unless* the run made
    /// no edits at all, in which case there is nothing to sample/select and we
    /// decline (re-running N identical no-edit attempts wastes budget).
    #[must_use]
    pub const fn is_hard(self) -> bool {
        if self.no_edits {
            return false;
        }
        self.tests_red || self.stuck || self.unmet || self.near_budget
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn cand(index: usize, outcome: TestOutcome, diff_len: usize, non_empty: bool) -> Candidate {
        Candidate {
            index,
            outcome,
            diff_len,
            non_empty,
        }
    }

    #[test]
    fn empty_slice_selects_nothing() {
        assert_eq!(select_best(&[]), None);
        assert!(!has_verified_winner(&[]));
    }

    #[test]
    fn a_passing_candidate_beats_failing_ones() {
        let cands = vec![
            cand(0, TestOutcome::Failed, 10, true),
            cand(1, TestOutcome::Passed, 500, true), // bigger diff but PASSES
            cand(2, TestOutcome::Unknown, 5, true),
        ];
        assert_eq!(select_best(&cands), Some(1));
        assert!(has_verified_winner(&cands));
    }

    #[test]
    fn among_passers_the_smallest_diff_wins() {
        let cands = vec![
            cand(0, TestOutcome::Passed, 300, true),
            cand(1, TestOutcome::Passed, 42, true), // smallest passing diff
            cand(2, TestOutcome::Passed, 100, true),
        ];
        assert_eq!(select_best(&cands), Some(1));
    }

    #[test]
    fn equal_passers_break_ties_by_lowest_index() {
        let cands = vec![
            cand(0, TestOutcome::Passed, 50, true),
            cand(1, TestOutcome::Passed, 50, true),
        ];
        assert_eq!(select_best(&cands), Some(0), "deterministic: lowest index");
    }

    #[test]
    fn empty_diff_never_beats_a_real_change() {
        // An empty candidate that "passes" (changed nothing) must not win over a
        // non-empty one — even a failing non-empty one is more useful to keep
        // iterating on than a no-op.
        let cands = vec![
            cand(0, TestOutcome::Passed, 0, false), // no-op "pass"
            cand(1, TestOutcome::Failed, 20, true), // real change, failing
        ];
        assert_eq!(select_best(&cands), Some(1));
        assert!(!has_verified_winner(&cands), "no verified NON-EMPTY winner");
    }

    #[test]
    fn all_failing_still_returns_least_bad_but_no_verified_winner() {
        let cands = vec![
            cand(0, TestOutcome::Failed, 200, true),
            cand(1, TestOutcome::Unknown, 10, true),
            cand(2, TestOutcome::Failed, 30, true), // smallest failing diff
        ];
        // Failed outranks Unknown; among the two Failed, the smaller wins.
        assert_eq!(select_best(&cands), Some(2));
        assert!(!has_verified_winner(&cands));
    }

    #[test]
    fn difficulty_gate_fires_on_any_signal() {
        assert!(DifficultySignals {
            tests_red: true,
            ..Default::default()
        }
        .is_hard());
        assert!(DifficultySignals {
            stuck: true,
            ..Default::default()
        }
        .is_hard());
        assert!(DifficultySignals {
            unmet: true,
            ..Default::default()
        }
        .is_hard());
        assert!(DifficultySignals {
            near_budget: true,
            ..Default::default()
        }
        .is_hard());
    }

    #[test]
    fn difficulty_gate_declines_easy_and_no_edit_runs() {
        // No signal at all ⇒ not hard.
        assert!(!DifficultySignals::default().is_hard());
        // Hard signals but NO edits ⇒ veto (nothing to select among).
        assert!(!DifficultySignals {
            tests_red: true,
            stuck: true,
            no_edits: true,
            ..Default::default()
        }
        .is_hard());
    }
}
