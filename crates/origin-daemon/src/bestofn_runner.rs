// SPDX-License-Identifier: Apache-2.0
//! Best-of-N orchestration (#5 / SWE-bench proposal 2.3) — daemon side.
//!
//! The pure selection policy lives in [`origin_swarm::bestofn`]; this module is
//! the *effectful* wrapper that, for a **hard** instance, runs N candidate
//! attempts in isolated git worktrees, scores each against the test oracle, and
//! applies the verified winner's diff to the user's tree.
//!
//! It is strictly opt-in (`ORIGIN_BESTOFN=N`, N≥2) and gated by difficulty so it
//! only spends N× on the residual of instances single-shot fails on. When
//! disabled — the default — none of this runs and the single-shot path is
//! byte-identical.
//!
//! ## Testability
//!
//! All git/exec/agent effects go through the [`CandidateArena`] trait, so the
//! orchestration ([`run_best_of_n`]) is unit-tested offline with a scripted
//! mock. The production impl ([`WorktreeArena`]) drives `git worktree` via
//! [`origin_vcs::Worktree`] and runs the test command through a shell.

use origin_swarm::bestofn::{has_verified_winner, select_best, Candidate, DifficultySignals, TestOutcome};
use origin_vcs::GitRunner as _;

/// A candidate attempt's captured result: the unified diff it produced (as a
/// patch applyable to the base tree) and the oracle verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateResult {
    /// The candidate's diff vs the base commit (`git diff --cached <base>`),
    /// empty when the attempt changed nothing.
    pub diff: String,
    /// The test-oracle verdict for the candidate's worktree.
    pub outcome: TestOutcome,
}

/// The effect surface best-of-N needs, behind a trait so the policy is testable.
///
/// One "arena" manages the lifecycle of the N isolated candidate checkouts.
/// Implementors are responsible for cleanup (worktree teardown) on drop.
#[allow(clippy::module_name_repetitions)]
pub trait CandidateArena {
    /// Produce candidate `index` (0-based): spin up an isolated checkout at the
    /// base commit, run the goal there, run the test oracle, and return the
    /// captured diff + verdict. Errors are surfaced as a string for logging; a
    /// failed *attempt* (vs a failed test) should map to an `Unknown` outcome
    /// rather than an `Err`, so one broken worktree doesn't abort the whole run.
    ///
    /// # Errors
    /// Returns `Err(String)` when the candidate could not be *set up or run at
    /// all* (e.g. the worktree could not be created); a test *failure* is a
    /// successful [`CandidateResult`] with a non-`Passed` outcome, not an `Err`.
    fn run_candidate(&mut self, index: usize) -> Result<CandidateResult, String>;

    /// Apply the winning `diff` to the user's real working tree. Called at most
    /// once, only for a verified winner.
    ///
    /// # Errors
    /// Returns `Err(String)` when the patch does not apply cleanly (or is empty);
    /// the caller then keeps the single-shot result rather than a partial apply.
    fn apply_winner(&mut self, diff: &str) -> Result<(), String>;
}

/// Outcome of a best-of-N run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BestOfNOutcome {
    /// Best-of-N was not attempted (disabled, not hard, or N<2). The caller
    /// keeps the single-shot result untouched.
    Skipped,
    /// A verified (passing, non-empty) winner was found and applied. Carries the
    /// winning candidate index and how many candidates passed.
    Applied { winner: usize, passed: usize },
    /// N candidates ran but none was a verified winner; nothing was applied and
    /// the caller keeps the single-shot result.
    NoWinner { attempted: usize },
}

/// Parse and clamp the `ORIGIN_BESTOFN` env var into an N ≥ 2 candidate count.
///
/// Unset / `0` / `1` / non-numeric ⇒ `None` (feature off). Values are clamped to
/// [`MAX_CANDIDATES`] so a typo can't fork a hundred worktrees.
#[must_use]
pub fn configured_n() -> Option<u32> {
    let raw = std::env::var("ORIGIN_BESTOFN").ok()?;
    let n: u32 = raw.trim().parse().ok()?;
    (n >= 2).then(|| n.min(MAX_CANDIDATES))
}

/// Hard ceiling on candidate count regardless of `ORIGIN_BESTOFN` — best-of-N
/// multiplies cost linearly, so we bound the blast radius.
pub const MAX_CANDIDATES: u32 = 6;

/// Run best-of-N over `arena`, gated by `n` and `difficulty`.
///
/// Returns [`BestOfNOutcome::Skipped`] without touching the arena when `n < 2`
/// or the instance isn't hard. Otherwise it runs `n` candidates, selects the
/// winner via the pure [`select_best`] policy, and — only for a verified winner
/// — calls [`CandidateArena::apply_winner`].
///
/// A candidate whose `run_candidate` errors is recorded as an `Unknown`,
/// empty-diff candidate (so it can never win) and the run continues; one broken
/// worktree never aborts the batch.
pub fn run_best_of_n(
    arena: &mut dyn CandidateArena,
    n: u32,
    difficulty: DifficultySignals,
) -> BestOfNOutcome {
    if n < 2 || !difficulty.is_hard() {
        return BestOfNOutcome::Skipped;
    }
    let mut results: Vec<CandidateResult> = Vec::with_capacity(n as usize);
    let mut candidates: Vec<Candidate> = Vec::with_capacity(n as usize);
    for i in 0..n as usize {
        let res = match arena.run_candidate(i) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(candidate = i, error = %e, "best-of-N: candidate attempt failed; scoring as Unknown");
                CandidateResult {
                    diff: String::new(),
                    outcome: TestOutcome::Unknown,
                }
            }
        };
        candidates.push(Candidate {
            index: i,
            outcome: res.outcome,
            diff_len: res.diff.len(),
            non_empty: !res.diff.trim().is_empty(),
        });
        results.push(res);
    }

    let passed = candidates.iter().filter(|c| c.outcome.is_passed() && c.non_empty).count();
    if !has_verified_winner(&candidates) {
        tracing::info!(attempted = n, "best-of-N: no verified winner; keeping single-shot result");
        return BestOfNOutcome::NoWinner { attempted: n as usize };
    }
    // `select_best` returns `Some` because the slice is non-empty; the winner is
    // a verified one because `has_verified_winner` held (the ordering puts a
    // passing non-empty candidate strictly above any non-passing/empty one).
    let Some(winner) = select_best(&candidates) else {
        return BestOfNOutcome::NoWinner { attempted: n as usize };
    };
    let diff = &results[winner].diff;
    if let Err(e) = arena.apply_winner(diff) {
        tracing::warn!(winner, error = %e, "best-of-N: applying the winning diff failed; keeping single-shot result");
        return BestOfNOutcome::NoWinner { attempted: n as usize };
    }
    tracing::info!(winner, passed, attempted = n, "best-of-N: applied verified winner");
    BestOfNOutcome::Applied {
        winner,
        passed,
    }
}

/// Async orchestration: like [`run_best_of_n`], but candidate production and
/// winner-application are `async` closures the caller awaits inline.
///
/// This is the production path: each candidate runs a full async `run_loop` in
/// its worktree, which cannot be expressed through the sync [`CandidateArena`]
/// trait. The batch is still serialized by the caller (best-of-N holds the
/// process-wide cwd lock), so awaiting candidates sequentially is correct. The
/// *decision* logic is shared verbatim with the sync path via [`select_best`] /
/// [`has_verified_winner`], so both are covered by the same policy tests.
///
/// * `run_one(i)` → the captured diff + oracle verdict for candidate `i`
///   (`Err` ⇒ scored `Unknown`/empty, batch continues).
/// * `apply(diff)` → apply the winning patch to the user's tree.
pub async fn run_best_of_n_async<RunFut, ApplyFut>(
    n: u32,
    difficulty: DifficultySignals,
    mut run_one: impl FnMut(usize) -> RunFut,
    apply: impl FnOnce(String) -> ApplyFut,
) -> BestOfNOutcome
where
    RunFut: std::future::Future<Output = Result<CandidateResult, String>>,
    ApplyFut: std::future::Future<Output = Result<(), String>>,
{
    if n < 2 || !difficulty.is_hard() {
        return BestOfNOutcome::Skipped;
    }
    let mut results: Vec<CandidateResult> = Vec::with_capacity(n as usize);
    let mut candidates: Vec<Candidate> = Vec::with_capacity(n as usize);
    for i in 0..n as usize {
        let res = match run_one(i).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(candidate = i, error = %e, "best-of-N: candidate attempt failed; scoring as Unknown");
                CandidateResult {
                    diff: String::new(),
                    outcome: TestOutcome::Unknown,
                }
            }
        };
        candidates.push(Candidate {
            index: i,
            outcome: res.outcome,
            diff_len: res.diff.len(),
            non_empty: !res.diff.trim().is_empty(),
        });
        results.push(res);
    }
    let passed = candidates.iter().filter(|c| c.outcome.is_passed() && c.non_empty).count();
    if !has_verified_winner(&candidates) {
        tracing::info!(attempted = n, "best-of-N: no verified winner; keeping single-shot result");
        return BestOfNOutcome::NoWinner { attempted: n as usize };
    }
    let Some(winner) = select_best(&candidates) else {
        return BestOfNOutcome::NoWinner { attempted: n as usize };
    };
    if let Err(e) = apply(results[winner].diff.clone()).await {
        tracing::warn!(winner, error = %e, "best-of-N: applying the winning diff failed; keeping single-shot result");
        return BestOfNOutcome::NoWinner { attempted: n as usize };
    }
    tracing::info!(winner, passed, attempted = n, "best-of-N: applied verified winner");
    BestOfNOutcome::Applied { winner, passed }
}

/// Process-wide guard held for the duration of a best-of-N batch.
////// The candidate agent runs edit files relative to the process **current
/// directory** (the tool builtins use cwd-relative paths), so a candidate must
/// run with cwd pointed at its worktree. `set_current_dir` is process-global, so
/// we serialize best-of-N against every other turn in the daemon while a batch
/// is in flight. Best-of-N is an opt-in, heavy, hard-instance path, so briefly
/// serializing the daemon is an acceptable trade for correctness.
static BESTOFN_CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The signature of the per-candidate agent runner the daemon supplies.
///
/// Given the candidate index and the worktree path (already the process cwd for
/// the duration of the call), it runs the goal to completion in that checkout.
/// It returns `Ok(())` on any terminal outcome (the *diff* is what we score, not
/// the loop's own success signal) or `Err` when the attempt could not run at all.
pub type CandidateRunner<'a> = dyn FnMut(usize, &std::path::Path) -> Result<(), String> + 'a;

/// Production [`CandidateArena`] backed by real `git worktree` lanes.
///
/// For each candidate it: adds a detached worktree at `base_commit` under a
/// temp root, swaps the process cwd into it (under [`BESTOFN_CWD_LOCK`]), invokes
/// the supplied [`CandidateRunner`], captures `git diff` of the result, runs the
/// test command, then tears the worktree down. The winning diff is applied to
/// the user's real tree with `git apply`.
pub struct WorktreeArena<'a> {
    /// Git driver rooted at the user's real workspace.
    git: &'a dyn origin_vcs::GitRunner,
    /// The base commit every candidate branches from (the pre-run HEAD).
    base_commit: String,
    /// Directory under which per-candidate worktrees are created.
    root: std::path::PathBuf,
    /// The test command scored per candidate; `None` ⇒ every candidate is
    /// `Unknown` (no oracle) and best-of-N will find no verified winner.
    test_command: Option<String>,
    /// The user's real workspace dir (cwd is restored to it after each candidate).
    workspace: std::path::PathBuf,
    /// The per-candidate agent runner supplied by the daemon.
    runner: Box<CandidateRunner<'a>>,
    /// Paths of worktrees created, for teardown on drop.
    created: Vec<std::path::PathBuf>,
}

impl<'a> WorktreeArena<'a> {
    /// Construct a worktree-backed arena.
    ///
    /// `git` must be rooted at `workspace`. `root` is where scratch worktrees are
    /// created (e.g. a tempdir). `base_commit` is the HEAD the run started from.
    pub fn new(
        git: &'a dyn origin_vcs::GitRunner,
        base_commit: impl Into<String>,
        root: impl Into<std::path::PathBuf>,
        workspace: impl Into<std::path::PathBuf>,
        test_command: Option<String>,
        runner: Box<CandidateRunner<'a>>,
    ) -> Self {
        Self {
            git,
            base_commit: base_commit.into(),
            root: root.into(),
            test_command,
            workspace: workspace.into(),
            runner,
            created: Vec::new(),
        }
    }

    /// Run the test command with `dir` as cwd; map the exit status to a verdict.
    fn score(dir: &std::path::Path, cmd: &str) -> TestOutcome {
        let run = if cfg!(windows) {
            std::process::Command::new("pwsh")
                .args(["-NoProfile", "-Command", cmd])
                .current_dir(dir)
                .output()
        } else {
            std::process::Command::new("sh")
                .args(["-c", cmd])
                .current_dir(dir)
                .output()
        };
        match run {
            Ok(out) if out.status.success() => TestOutcome::Passed,
            Ok(_) => TestOutcome::Failed,
            Err(_) => TestOutcome::Unknown,
        }
    }
}

impl CandidateArena for WorktreeArena<'_> {
    fn run_candidate(&mut self, index: usize) -> Result<CandidateResult, String> {
        let wt_path = self.root.join(format!("origin-bestofn-{index}"));
        let wt = origin_vcs::Worktree::new(self.git);
        // Detached worktree at the base commit (no branch churn in the user repo).
        wt.add_existing(&wt_path, &self.base_commit)
            .map_err(|e| format!("worktree add: {e}"))?;
        self.created.push(wt_path.clone());

        // Run the candidate with cwd pointed at the worktree. `set_current_dir`
        // is process-global ⇒ serialize the whole batch against other turns.
        let outcome;
        let diff;
        {
            let _cwd_guard = BESTOFN_CWD_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let prev = std::env::current_dir().map_err(|e| format!("getcwd: {e}"))?;
            std::env::set_current_dir(&wt_path).map_err(|e| format!("chdir worktree: {e}"))?;
            // Always restore cwd, even if the runner or scoring panics/early-returns.
            let restore = scopeguard_chdir(prev);
            (self.runner)(index, &wt_path)?;
            // Capture the candidate's change as a patch against the base commit.
            let wt_git = origin_daemon_git_at(&wt_path);
            // Stage everything so new files are included, then diff vs base.
            let _ = wt_git.run(&["add", "-A"]);
            diff = wt_git
                .run(&["diff", "--cached", &self.base_commit])
                .unwrap_or_default();
            outcome = self
                .test_command
                .as_deref()
                .map_or(TestOutcome::Unknown, |cmd| Self::score(&wt_path, cmd));
            drop(restore);
        }
        Ok(CandidateResult { diff, outcome })
    }

    fn apply_winner(&mut self, diff: &str) -> Result<(), String> {
        if diff.trim().is_empty() {
            return Err("winning diff is empty".into());
        }
        // Apply the winning patch to the user's real tree. `git apply` reads the
        // patch from stdin; route it through a temp file to avoid stdin plumbing.
        let tmp = self.root.join("origin-bestofn-winner.patch");
        std::fs::write(&tmp, diff).map_err(|e| format!("write patch: {e}"))?;
        let out = std::process::Command::new("git")
            .args(["apply", "--whitespace=nowarn"])
            .arg(&tmp)
            .current_dir(&self.workspace)
            .output()
            .map_err(|e| format!("spawn git apply: {e}"))?;
        let _ = std::fs::remove_file(&tmp);
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}

impl Drop for WorktreeArena<'_> {
    fn drop(&mut self) {
        let wt = origin_vcs::Worktree::new(self.git);
        for path in &self.created {
            let _ = wt.remove(path, true);
        }
        let _ = wt.prune();
    }
}

/// A tiny cwd-restoring guard (avoids pulling in the `scopeguard` crate).
struct ChdirRestore(Option<std::path::PathBuf>);
const fn scopeguard_chdir(prev: std::path::PathBuf) -> ChdirRestore {
    ChdirRestore(Some(prev))
}
impl Drop for ChdirRestore {
    fn drop(&mut self) {
        if let Some(prev) = self.0.take() {
            let _ = std::env::set_current_dir(prev);
        }
    }
}

/// A [`origin_vcs::GitRunner`] rooted at `dir` (for per-worktree git calls).
fn origin_daemon_git_at(dir: &std::path::Path) -> crate::selfdev::ProcessGitRunner {
    crate::selfdev::ProcessGitRunner::new(dir.to_path_buf())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::unnecessary_wraps)]
mod tests {
    use super::*;

    /// A scripted arena: each `run_candidate(i)` pops the pre-seeded result for
    /// `i`; records whether `apply_winner` ran and with which diff.
    struct MockArena {
        results: Vec<Result<CandidateResult, String>>,
        applied: Option<String>,
        apply_fails: bool,
    }
    impl MockArena {
        fn new(results: Vec<Result<CandidateResult, String>>) -> Self {
            Self {
                results,
                applied: None,
                apply_fails: false,
            }
        }
    }
    impl CandidateArena for MockArena {
        fn run_candidate(&mut self, index: usize) -> Result<CandidateResult, String> {
            self.results[index].clone()
        }
        fn apply_winner(&mut self, diff: &str) -> Result<(), String> {
            if self.apply_fails {
                return Err("apply failed".into());
            }
            self.applied = Some(diff.to_string());
            Ok(())
        }
    }

    fn pass(diff: &str) -> Result<CandidateResult, String> {
        Ok(CandidateResult {
            diff: diff.into(),
            outcome: TestOutcome::Passed,
        })
    }
    fn fail(diff: &str) -> Result<CandidateResult, String> {
        Ok(CandidateResult {
            diff: diff.into(),
            outcome: TestOutcome::Failed,
        })
    }
    fn hard() -> DifficultySignals {
        DifficultySignals {
            tests_red: true,
            ..Default::default()
        }
    }

    #[test]
    fn skips_when_n_below_two() {
        let mut a = MockArena::new(vec![pass("x")]);
        assert_eq!(run_best_of_n(&mut a, 1, hard()), BestOfNOutcome::Skipped);
        assert!(a.applied.is_none(), "must not touch the arena when skipped");
    }

    #[test]
    fn skips_when_not_hard() {
        let mut a = MockArena::new(vec![pass("x"), pass("y")]);
        assert_eq!(
            run_best_of_n(&mut a, 2, DifficultySignals::default()),
            BestOfNOutcome::Skipped
        );
        assert!(a.applied.is_none());
    }

    #[test]
    fn applies_the_passing_winner() {
        // Candidate 0 fails, candidate 1 passes ⇒ apply candidate 1's diff.
        let mut a = MockArena::new(vec![fail("bad-diff"), pass("good-diff")]);
        let out = run_best_of_n(&mut a, 2, hard());
        assert_eq!(out, BestOfNOutcome::Applied { winner: 1, passed: 1 });
        assert_eq!(a.applied.as_deref(), Some("good-diff"));
    }

    #[test]
    fn picks_smallest_passing_diff_across_three() {
        let mut a = MockArena::new(vec![
            pass("a-longer-diff-here"),
            pass("short"),
            pass("another-long-one"),
        ]);
        let out = run_best_of_n(&mut a, 3, hard());
        assert_eq!(out, BestOfNOutcome::Applied { winner: 1, passed: 3 });
        assert_eq!(a.applied.as_deref(), Some("short"));
    }

    #[test]
    fn no_winner_when_all_fail_applies_nothing() {
        let mut a = MockArena::new(vec![fail("x"), fail("y")]);
        let out = run_best_of_n(&mut a, 2, hard());
        assert_eq!(out, BestOfNOutcome::NoWinner { attempted: 2 });
        assert!(a.applied.is_none(), "nothing applied when no candidate passes");
    }

    #[test]
    fn a_broken_candidate_scores_unknown_and_run_continues() {
        // Candidate 0 errors (worktree blew up), candidate 1 passes ⇒ 1 wins.
        let mut a = MockArena::new(vec![Err("git exploded".into()), pass("good")]);
        let out = run_best_of_n(&mut a, 2, hard());
        assert_eq!(out, BestOfNOutcome::Applied { winner: 1, passed: 1 });
        assert_eq!(a.applied.as_deref(), Some("good"));
    }

    #[test]
    fn empty_diff_pass_is_not_a_verified_winner() {
        // A candidate that "passes" but changed nothing must not be applied.
        let mut a = MockArena::new(vec![pass("   "), fail("real-change")]);
        let out = run_best_of_n(&mut a, 2, hard());
        assert_eq!(out, BestOfNOutcome::NoWinner { attempted: 2 });
        assert!(a.applied.is_none());
    }

    #[test]
    fn apply_failure_degrades_to_no_winner() {
        let mut a = MockArena::new(vec![pass("good")]);
        a.results.push(fail("x")); // make it 2 candidates
        a.apply_fails = true;
        let out = run_best_of_n(&mut a, 2, hard());
        assert_eq!(out, BestOfNOutcome::NoWinner { attempted: 2 });
    }

    #[test]
    fn configured_n_parses_and_clamps() {
        // (env mutation is serialized by the module-local lock)
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("ORIGIN_BESTOFN", "3");
        assert_eq!(configured_n(), Some(3));
        std::env::set_var("ORIGIN_BESTOFN", "1");
        assert_eq!(configured_n(), None, "N<2 ⇒ off");
        std::env::set_var("ORIGIN_BESTOFN", "999");
        assert_eq!(configured_n(), Some(MAX_CANDIDATES), "clamped");
        std::env::remove_var("ORIGIN_BESTOFN");
        assert_eq!(configured_n(), None, "unset ⇒ off");
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
