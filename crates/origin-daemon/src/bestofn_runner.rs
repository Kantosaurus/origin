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
    /// C5: the diff modified or deleted a test file that existed before the
    /// run. The candidate was still scored, on the original tests.
    pub tampered: bool,
}

/// C5: `diff` with line endings, trailing whitespace and `index` header lines
/// (blob hashes, which change with the bytes) normalised away, so candidates
/// that made the same change compare equal.
fn normalize_diff(diff: &str) -> String {
    diff.lines()
        .filter(|l| !l.starts_with("index "))
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
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
    /// winning candidate index, how many candidates passed, and (C5) the
    /// candidates flagged for changing a pre-existing test — the winner's own
    /// test changes are part of the applied diff when it is among them.
    Applied {
        winner: usize,
        passed: usize,
        tampered: Vec<usize>,
    },
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
///
/// C5: candidates run one after another, so when the first two produce the
/// same (normalised) non-empty diff and it passes, the rest are not run.
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
                    tampered: false,
                }
            }
        };
        if res.tampered {
            tracing::warn!(
                candidate = i,
                "best-of-N: candidate changed a pre-existing test; scored on the original"
            );
        }
        candidates.push(Candidate {
            index: i,
            outcome: res.outcome,
            diff_len: res.diff.len(),
            non_empty: !res.diff.trim().is_empty(),
        });
        results.push(res);
        if i == 1
            && (n as usize) > 2
            && candidates.iter().all(|c| c.outcome.is_passed() && c.non_empty)
            && normalize_diff(&results[0].diff) == normalize_diff(&results[1].diff)
        {
            tracing::info!("best-of-N: the first two candidates agree and pass; not running the rest");
            break;
        }
    }

    let attempted = results.len();
    let passed = candidates.iter().filter(|c| c.outcome.is_passed() && c.non_empty).count();
    if !has_verified_winner(&candidates) {
        tracing::info!(
            attempted,
            "best-of-N: no verified winner; keeping single-shot result"
        );
        return BestOfNOutcome::NoWinner { attempted };
    }
    // `select_best` returns `Some` because the slice is non-empty; the winner is
    // a verified one because `has_verified_winner` held (the ordering puts a
    // passing non-empty candidate strictly above any non-passing/empty one).
    let Some(winner) = select_best(&candidates) else {
        return BestOfNOutcome::NoWinner { attempted };
    };
    let diff = &results[winner].diff;
    if let Err(e) = arena.apply_winner(diff) {
        tracing::warn!(winner, error = %e, "best-of-N: applying the winning diff failed; keeping single-shot result");
        return BestOfNOutcome::NoWinner { attempted };
    }
    let tampered: Vec<usize> = (0..attempted).filter(|&i| results[i].tampered).collect();
    tracing::info!(
        winner,
        passed,
        attempted,
        ?tampered,
        "best-of-N: applied verified winner"
    );
    BestOfNOutcome::Applied {
        winner,
        passed,
        tampered,
    }
}

/// The signature of the per-candidate agent runner the daemon supplies.
///
/// Given the candidate index and the worktree path, it runs the goal to
/// completion in that checkout (A12: by passing the path as the run's working
/// directory, never by changing the process-wide cwd).
/// It returns `Ok(())` on any terminal outcome (the *diff* is what we score, not
/// the loop's own success signal) or `Err` when the attempt could not run at all.
pub type CandidateRunner<'a> = dyn FnMut(usize, &std::path::Path) -> Result<(), String> + 'a;

/// Production [`CandidateArena`] backed by real `git worktree` lanes.
///
/// For each candidate it: adds a detached worktree at `base_commit` under a
/// temp root, invokes the supplied [`CandidateRunner`] with the worktree path,
/// captures `git diff` of the result, runs the
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
    /// The user's real workspace dir (the winning diff is applied here).
    workspace: std::path::PathBuf,
    /// The per-candidate agent runner supplied by the daemon.
    runner: Box<CandidateRunner<'a>>,
    /// Paths of worktrees created, for teardown on drop.
    created: Vec<std::path::PathBuf>,
    /// C5: verdict + tamper flag per normalised diff already scored, so an
    /// identical candidate shares it instead of re-running the tests.
    scored: std::collections::HashMap<String, (TestOutcome, bool)>,
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
            scored: std::collections::HashMap::new(),
        }
    }

    /// C5: score `cmd` in the worktree `wt` on the tests that existed at the
    /// base commit: a pre-existing test file the candidate modified or deleted
    /// is restored, a test file it added is set aside; afterwards its own
    /// versions are put back (its diff was captured before). Returns the
    /// verdict and whether a pre-existing test was changed. A restore that
    /// fails scores `Unknown`, never a pass on tampered tests.
    // ponytail: "test file" is `origin_postedit::is_test_file`'s naming
    // convention — a changed helper/fixture outside it (conftest.py, testdata)
    // is not restored; a runner-reported list of collected files is the upgrade.
    fn score_on_base_tests(
        &self,
        git: &dyn origin_vcs::GitRunner,
        wt: &std::path::Path,
        cmd: &str,
    ) -> (TestOutcome, bool) {
        // `-z`: NUL-separated `status, path` pairs, no path quoting;
        // `--no-renames`: a renamed test reads as deleted + added.
        let base = self.base_commit.as_str();
        let listing = git
            .run(&["diff", "--cached", "--name-status", "--no-renames", "-z", base])
            .unwrap_or_default();
        let fields: Vec<&str> = listing.split('\0').collect();
        let (mut existing, mut added): (Vec<(&str, &str)>, Vec<&str>) = (Vec::new(), Vec::new());
        for pair in fields.chunks_exact(2) {
            if !origin_postedit::is_test_file(pair[1]) {
                continue;
            }
            if pair[0] == "A" {
                added.push(pair[1]);
            } else {
                existing.push((pair[0], pair[1]));
            }
        }
        let tampered = !existing.is_empty();
        let mut ok = true;
        if tampered {
            let mut args = vec!["restore", "--source", base, "--worktree", "--"];
            args.extend(existing.iter().map(|&(_, p)| p));
            ok &= git.run(&args).is_ok();
        }
        for p in &added {
            ok &= std::fs::remove_file(wt.join(p)).is_ok();
        }
        let outcome = if ok {
            Self::score(wt, cmd)
        } else {
            TestOutcome::Unknown
        };
        // Put the candidate's versions back: the index holds its modified and
        // added files; a test it deleted is not in the index, so delete it again.
        let staged: Vec<&str> = existing
            .iter()
            .filter(|&&(s, _)| s != "D")
            .map(|&(_, p)| p)
            .chain(added.iter().copied())
            .collect();
        if !staged.is_empty() {
            let mut args = vec!["checkout", "--"];
            args.extend(staged);
            let _ = git.run(&args);
        }
        for &(_, p) in existing.iter().filter(|&&(s, _)| s == "D") {
            let _ = std::fs::remove_file(wt.join(p));
        }
        (outcome, tampered)
    }

    /// Run the test command with `dir` as cwd; map the exit status to a verdict.
    /// A6: spawned exactly like the turn-end gates (cmd guard, scrubbed child
    /// env, PowerShell fallback when `pwsh` is absent); a guard denial, spawn
    /// failure or the 600s bound scores `Unknown`.
    fn score(dir: &std::path::Path, cmd: &str) -> TestOutcome {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
            return TestOutcome::Unknown;
        };
        match rt.block_on(crate::agent::run_gate_command(cmd, Some(dir))) {
            Some((0, _)) => TestOutcome::Passed,
            Some(_) => TestOutcome::Failed,
            None => TestOutcome::Unknown,
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

        // A12: the runner gets the worktree path and works there; the process
        // cwd is never moved (it is shared by every session in the daemon).
        (self.runner)(index, &wt_path)?;
        // Capture the candidate's change as a patch against the base commit.
        let wt_git = origin_daemon_git_at(&wt_path);
        // Stage everything so new files are included, then diff vs base.
        let _ = wt_git.run(&["add", "-A"]);
        let diff = wt_git
            .run(&["diff", "--cached", &self.base_commit])
            .unwrap_or_default();
        let Some(cmd) = self.test_command.as_deref() else {
            return Ok(CandidateResult {
                diff,
                outcome: TestOutcome::Unknown,
                tampered: false,
            });
        };
        // C5: an identical change was scored already ⇒ share its verdict.
        let key = normalize_diff(&diff);
        let (outcome, tampered) = if let Some(&hit) = self.scored.get(&key) {
            hit
        } else {
            let scored = self.score_on_base_tests(&wt_git, &wt_path, cmd);
            // A gate that did not run is not cached: the next one may run.
            if scored.0 != TestOutcome::Unknown {
                self.scored.insert(key, scored);
            }
            scored
        };
        Ok(CandidateResult {
            diff,
            outcome,
            tampered,
        })
    }

    fn apply_winner(&mut self, diff: &str) -> Result<(), String> {
        if diff.trim().is_empty() {
            return Err("winning diff is empty".into());
        }
        // Apply the winning patch to the user's real tree. `git apply` reads the
        // patch from stdin; route it through a temp file to avoid stdin plumbing.
        let tmp = self.root.join("origin-bestofn-winner.patch");
        std::fs::write(&tmp, diff).map_err(|e| format!("write patch: {e}"))?;
        let mut git = std::process::Command::new("git");
        origin_tools::scrub_env(&mut git); // A6
        let out = git
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
        runs: usize,
    }
    impl MockArena {
        fn new(results: Vec<Result<CandidateResult, String>>) -> Self {
            Self {
                results,
                applied: None,
                apply_fails: false,
                runs: 0,
            }
        }
    }
    impl CandidateArena for MockArena {
        fn run_candidate(&mut self, index: usize) -> Result<CandidateResult, String> {
            self.runs += 1;
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
            tampered: false,
        })
    }
    fn fail(diff: &str) -> Result<CandidateResult, String> {
        Ok(CandidateResult {
            diff: diff.into(),
            outcome: TestOutcome::Failed,
            tampered: false,
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
        assert_eq!(
            out,
            BestOfNOutcome::Applied {
                winner: 1,
                passed: 1,
                tampered: vec![]
            }
        );
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
        assert_eq!(
            out,
            BestOfNOutcome::Applied {
                winner: 1,
                passed: 3,
                tampered: vec![]
            }
        );
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
        assert_eq!(
            out,
            BestOfNOutcome::Applied {
                winner: 1,
                passed: 1,
                tampered: vec![]
            }
        );
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
    fn stops_early_when_the_first_two_agree_and_pass() {
        // Same change, CRLF vs LF and trailing whitespace ⇒ identical after
        // normalisation; candidate 2 is never run.
        let mut a = MockArena::new(vec![pass("+fix\n"), pass("+fix  \r\n"), pass("other")]);
        let out = run_best_of_n(&mut a, 3, hard());
        assert_eq!(a.runs, 2, "the third candidate is not run");
        assert!(
            matches!(
                out,
                BestOfNOutcome::Applied {
                    winner: 0,
                    passed: 2,
                    ..
                }
            ),
            "{out:?}"
        );
    }

    #[test]
    fn no_early_stop_unless_both_pass_with_the_same_diff() {
        for first_two in [
            vec![fail("+fix"), fail("+fix")],
            vec![
                Ok(CandidateResult {
                    diff: "+fix".into(),
                    outcome: TestOutcome::Unknown,
                    tampered: false,
                }),
                Ok(CandidateResult {
                    diff: "+fix".into(),
                    outcome: TestOutcome::Unknown,
                    tampered: false,
                }),
            ],
            vec![pass("+fix"), pass("+other")],
        ] {
            let mut results = first_two;
            results.push(pass("+third"));
            let mut a = MockArena::new(results);
            let _ = run_best_of_n(&mut a, 3, hard());
            assert_eq!(a.runs, 3);
        }
    }

    #[test]
    fn apply_failure_degrades_to_no_winner() {
        let mut a = MockArena::new(vec![pass("good")]);
        a.results.push(fail("x")); // make it 2 candidates
        a.apply_fails = true;
        let out = run_best_of_n(&mut a, 2, hard());
        assert_eq!(out, BestOfNOutcome::NoWinner { attempted: 2 });
    }

    /// A12: a candidate runs in its own worktree (passed to the runner) without
    /// moving the process-wide cwd, which every other session in the daemon
    /// resolves relative paths against.
    #[test]
    fn running_a_candidate_leaves_the_process_cwd_unchanged() {
        let repo = tempfile::tempdir().unwrap();
        let git = crate::selfdev::ProcessGitRunner::new(repo.path());
        git.run(&["init", "-q"]).unwrap();
        git.run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "base",
        ])
        .unwrap();
        let base = git.run(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let scratch = tempfile::tempdir().unwrap();
        let before = std::env::current_dir().unwrap();
        let seen = std::cell::RefCell::new(None);
        let runner = |_i: usize, wt: &std::path::Path| -> Result<(), String> {
            *seen.borrow_mut() = Some(std::env::current_dir().unwrap());
            std::fs::write(wt.join("new.txt"), "hi\n").map_err(|e| e.to_string())
        };
        let mut arena = WorktreeArena::new(&git, base, scratch.path(), repo.path(), None, Box::new(runner));
        let res = arena.run_candidate(0).unwrap();
        drop(arena);
        assert_eq!(
            seen.into_inner(),
            Some(before),
            "the candidate must run without changing the process cwd"
        );
        assert!(
            res.diff.contains("new.txt"),
            "the worktree edit is captured: {}",
            res.diff
        );
    }

    /// A6: the test oracle is spawned like the turn-end gates — in the given
    /// directory, with the daemon's credentials scrubbed from its env, and via
    /// the PowerShell fallback on a Windows host without `pwsh`.
    #[test]
    fn score_runs_in_dir_with_a_scrubbed_env() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker"), "").unwrap();
        #[cfg(unix)]
        let cmd = r#"test -f marker && test -z "$A6_SCORE_PROBE_API_KEY""#;
        #[cfg(windows)]
        let cmd = "if (-not (Test-Path marker) -or $env:A6_SCORE_PROBE_API_KEY) { exit 1 }";
        std::env::set_var("A6_SCORE_PROBE_API_KEY", "score-secret");
        let outcome = WorktreeArena::score(dir.path(), cmd);
        std::env::remove_var("A6_SCORE_PROBE_API_KEY");
        assert_eq!(outcome, TestOutcome::Passed);
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
