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
    /// C5: the pre-existing test files the diff modified or deleted (empty ⇒
    /// not tampered). The candidate was still scored, on the original tests.
    pub tampered: Vec<String>,
    /// C5: every path the diff changes is a test file. Such a candidate fixed
    /// nothing, so it is never a verified winner, whatever it scored.
    pub tests_only: bool,
}

/// C5: `diff` with line endings, trailing whitespace and `index` header lines
/// (blob hashes, which change with the bytes) normalised away, so candidates
/// that made the same change compare equal. A binary file's section keeps its
/// `index` line: there it is the only thing that tells two changes apart.
fn normalize_diff(diff: &str) -> String {
    fn flush(section: &mut Vec<&str>, out: &mut Vec<String>) {
        let binary = section
            .iter()
            .any(|l| l.starts_with("Binary files ") || *l == "GIT binary patch");
        out.extend(
            section
                .drain(..)
                .filter(|l| binary || !l.starts_with("index "))
                .map(|l| l.trim_end().to_string()),
        );
    }
    let (mut out, mut section) = (Vec::new(), Vec::new());
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            flush(&mut section, &mut out);
        }
        section.push(line);
    }
    flush(&mut section, &mut out);
    out.join("\n")
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
    /// winning candidate index, how many candidates passed, and (C5) each
    /// candidate flagged for changing pre-existing tests, with those test paths
    /// — the winner's own test changes are part of the applied diff when it is
    /// among them.
    Applied {
        winner: usize,
        passed: usize,
        tampered: Vec<(usize, Vec<String>)>,
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
    run_best_of_n_noted(arena, n, difficulty).0
}

/// C5: [`run_best_of_n`], then tell the user how it went.
///
/// ONE `[origin]` line on `event_tx` (never into the session) once it ran:
/// which candidate was applied and why, or why none was; candidates that
/// changed pre-existing tests (up to five paths each); candidates excluded as
/// tests-only. Call it off the async runtime (it uses `blocking_send`).
pub fn run_best_of_n_reported(
    arena: &mut dyn CandidateArena,
    n: u32,
    difficulty: DifficultySignals,
    event_tx: Option<&tokio::sync::mpsc::Sender<crate::protocol::StreamEvent>>,
) -> BestOfNOutcome {
    let (outcome, note) = run_best_of_n_noted(arena, n, difficulty);
    if let (Some(tx), Some(text)) = (event_tx, note) {
        let text = crate::redact::string(text);
        let _ = tx.blocking_send(crate::protocol::StreamEvent::TextDelta { text });
    }
    outcome
}

/// C5: the user-facing note on a finished run (candidates counted 1-based):
/// the applied winner (`Ok(index)`) or why none was applied, then the
/// pre-existing tests each candidate changed (up to five paths, then
/// `+K more`) and the candidates excluded as tests-only.
fn outcome_note(results: &[CandidateResult], passed: usize, applied: Result<usize, String>) -> String {
    use std::fmt::Write as _;
    let attempted = results.len();
    let mut out = match applied {
        Ok(w) => format!(
            "\n\n[origin] Best-of-N applied candidate {} of {attempted} to your working tree: it passed \
             the tests with the smallest change ({} diff bytes; {passed} of {attempted} candidates passed).",
            w + 1,
            results[w].diff.len()
        ),
        Err(why) => format!(
            "\n\n[origin] Best-of-N: none of {attempted} candidates was applied ({why}); your working \
             tree is as the first attempt left it."
        ),
    };
    for (i, r) in results.iter().enumerate().filter(|(_, r)| !r.tampered.is_empty()) {
        let shown = r.tampered.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
        let more = r.tampered.len().saturating_sub(5);
        let more = if more > 0 {
            format!(" (+{more} more)")
        } else {
            String::new()
        };
        let _ = write!(
            out,
            " Candidate {} changed pre-existing tests: {shown}{more} (scored on the originals).",
            i + 1
        );
    }
    let tests_only: Vec<String> = results
        .iter()
        .enumerate()
        .filter(|(_, r)| r.tests_only)
        .map(|(i, _)| format!("candidate {}", i + 1))
        .collect();
    if !tests_only.is_empty() {
        let _ = write!(
            out,
            " Excluded as tests-only (changed only test files): {}.",
            tests_only.join(", ")
        );
    }
    out
}

/// [`run_best_of_n`] and, once it ran, its user-facing note.
fn run_best_of_n_noted(
    arena: &mut dyn CandidateArena,
    n: u32,
    difficulty: DifficultySignals,
) -> (BestOfNOutcome, Option<String>) {
    if n < 2 || !difficulty.is_hard() {
        return (BestOfNOutcome::Skipped, None);
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
                    tampered: Vec::new(),
                    tests_only: false,
                }
            }
        };
        if !res.tampered.is_empty() {
            tracing::warn!(
                candidate = i,
                tests = ?res.tampered,
                "best-of-N: candidate changed pre-existing tests; scored on the originals"
            );
        }
        if res.tests_only {
            tracing::info!(
                candidate = i,
                "best-of-N: candidate changed only tests; not a winner"
            );
        }
        candidates.push(Candidate {
            index: i,
            outcome: res.outcome,
            diff_len: res.diff.len(),
            // A tests-only change fixed nothing: select it like an empty diff.
            non_empty: !res.diff.trim().is_empty() && !res.tests_only,
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
    let no_winner = |why: String| {
        (
            BestOfNOutcome::NoWinner { attempted },
            Some(outcome_note(&results, passed, Err(why))),
        )
    };
    if !has_verified_winner(&candidates) {
        tracing::info!(
            attempted,
            "best-of-N: no verified winner; keeping single-shot result"
        );
        return no_winner("no candidate passed the tests with a change outside the tests".into());
    }
    // `select_best` returns `Some` because the slice is non-empty; the winner is
    // a verified one because `has_verified_winner` held (the ordering puts a
    // passing non-empty candidate strictly above any non-passing/empty one).
    let Some(winner) = select_best(&candidates) else {
        return no_winner("no candidate could be selected".into());
    };
    let diff = &results[winner].diff;
    if let Err(e) = arena.apply_winner(diff) {
        tracing::warn!(winner, error = %e, "best-of-N: applying the winning diff failed; keeping single-shot result");
        return no_winner(format!(
            "candidate {} won, but applying its diff failed: {e}",
            winner + 1
        ));
    }
    let note = outcome_note(&results, passed, Ok(winner));
    let tampered: Vec<(usize, Vec<String>)> = results
        .into_iter()
        .enumerate()
        .filter(|(_, r)| !r.tampered.is_empty())
        .map(|(i, r)| (i, r.tampered))
        .collect();
    tracing::info!(
        winner,
        passed,
        attempted,
        ?tampered,
        "best-of-N: applied verified winner"
    );
    (
        BestOfNOutcome::Applied {
            winner,
            passed,
            tampered,
        },
        Some(note),
    )
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
    scored: std::collections::HashMap<String, CandidateResult>,
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
    /// verdict, the pre-existing tests it changed, and whether it changed only
    /// tests (`diff` is left empty for the caller). A change list that cannot
    /// be read, or a restore that fails, scores `Unknown`, never a pass on
    /// tampered tests.
    // ponytail: "test file" is `origin_postedit::is_test_file`'s naming
    // convention, so this cannot see tampering with: inline `#[cfg(test)]`
    // modules, Go `testdata/`, a root `conftest.py` or other helper/fixture
    // outside a test dir, runner configuration (`pytest.ini`, jest config), or
    // an added `#[ignore]`/skip marker inside a source file. A runner-reported
    // list of collected files is the upgrade path.
    fn score_on_base_tests(
        &self,
        git: &dyn origin_vcs::GitRunner,
        wt: &std::path::Path,
        cmd: &str,
    ) -> CandidateResult {
        // Paths per git call: far below the Windows command-line limit.
        const PATHS_PER_CALL: usize = 100;
        let base = self.base_commit.as_str();
        let unknown = CandidateResult {
            diff: String::new(),
            outcome: TestOutcome::Unknown,
            tampered: Vec::new(),
            tests_only: false,
        };
        // `-z`: NUL-separated `status, path` pairs, no path quoting;
        // `--no-renames`: a renamed test reads as deleted + added.
        let Ok(listing) = git.run(&["diff", "--cached", "--name-status", "--no-renames", "-z", base]) else {
            return unknown;
        };
        let fields: Vec<&str> = listing.split('\0').collect();
        let pairs: Vec<&[&str]> = fields.chunks_exact(2).collect();
        let tests_only = !pairs.is_empty() && pairs.iter().all(|p| origin_postedit::is_test_file(p[1]));
        let (mut existing, mut added): (Vec<(&str, &str)>, Vec<&str>) = (Vec::new(), Vec::new());
        for pair in pairs.iter().filter(|p| origin_postedit::is_test_file(p[1])) {
            if pair[0] == "A" {
                added.push(pair[1]);
            } else {
                existing.push((pair[0], pair[1]));
            }
        }
        // `--literal-pathspecs`: a file name with `*`/`?`/`[` is a name, not a glob.
        let run_chunked = |head: &[&str], paths: &[&str]| {
            paths.chunks(PATHS_PER_CALL).fold(true, |ok, chunk| {
                let mut args = vec!["--literal-pathspecs"];
                args.extend_from_slice(head);
                args.push("--");
                args.extend_from_slice(chunk);
                git.run(&args).is_ok() && ok
            })
        };
        let changed: Vec<&str> = existing.iter().map(|&(_, p)| p).collect();
        let mut ok = run_chunked(&["restore", "--source", base, "--worktree"], &changed);
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
        let _ = run_chunked(&["checkout"], &staged);
        for &(_, p) in existing.iter().filter(|&&(s, _)| s == "D") {
            let _ = std::fs::remove_file(wt.join(p));
        }
        CandidateResult {
            outcome,
            tampered: changed.into_iter().map(str::to_string).collect(),
            tests_only,
            ..unknown
        }
    }

    /// C5: `Err` when a file the patch at `patch` touches has uncommitted
    /// changes (or is untracked) in the user's workspace, or that cannot be
    /// determined.
    fn refuse_dirty_overlap(&self, patch: &std::path::Path) -> Result<(), String> {
        let patch = patch.to_string_lossy();
        // `-z`: `added\tdeleted\tpath\0`, or for a rename an empty path field
        // followed by `old\0new\0`.
        let stat = self
            .git
            .run(&["apply", "--numstat", "-z", &patch])
            .map_err(|e| format!("could not list the winning diff's files: {e}"))?;
        let paths: Vec<&str> = stat
            .split('\0')
            .map(|f| f.rsplit('\t').next().unwrap_or(f))
            .filter(|p| !p.is_empty())
            .collect();
        if paths.is_empty() {
            return Ok(());
        }
        let mut args = vec![
            "--literal-pathspecs",
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=all",
            "--",
        ];
        args.extend(paths.iter().copied());
        let status = self
            .git
            .run(&args)
            .map_err(|e| format!("could not check the working tree: {e}"))?;
        // `XY path\0` (a rename adds `orig\0`): the path after the status.
        let dirty: Vec<&str> = status
            .split('\0')
            .filter_map(|e| e.get(3..))
            .filter(|p| !p.is_empty())
            .collect();
        if dirty.is_empty() {
            return Ok(());
        }
        let more = dirty.len().saturating_sub(5);
        let more = if more > 0 {
            format!(" (+{more} more)")
        } else {
            String::new()
        };
        Err(format!(
            "the working tree has uncommitted changes to {}{more}, which the winning diff also \
             changes; it was made against HEAD, so it was not applied",
            dirty.iter().take(5).copied().collect::<Vec<_>>().join(", ")
        ))
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
                tampered: Vec::new(),
                tests_only: false,
            });
        };
        // C5: an identical change was scored already ⇒ share its verdict.
        let key = normalize_diff(&diff);
        let scored = if let Some(hit) = self.scored.get(&key) {
            hit.clone()
        } else {
            let scored = self.score_on_base_tests(&wt_git, &wt_path, cmd);
            // A gate that did not run is not cached: the next one may run.
            if scored.outcome != TestOutcome::Unknown {
                self.scored.insert(key, scored.clone());
            }
            scored
        };
        Ok(CandidateResult { diff, ..scored })
    }

    fn apply_winner(&mut self, diff: &str) -> Result<(), String> {
        if diff.trim().is_empty() {
            return Err("winning diff is empty".into());
        }
        // Apply the winning patch to the user's real tree. `git apply` reads the
        // patch from stdin; route it through a temp file to avoid stdin plumbing.
        let tmp = self.root.join("origin-bestofn-winner.patch");
        std::fs::write(&tmp, diff).map_err(|e| format!("write patch: {e}"))?;
        // C5: the diff is against the base commit, but the first attempt's
        // edits are still uncommitted in the user's tree. Refuse a diff that
        // touches any of those files (git would fail on the same lines, or
        // silently mix both edits in one file); fail closed if unsure.
        // ponytail: a diff touching only OTHER files still lands on top of
        // the first attempt's edits elsewhere — an untested mix; deciding
        // whether to discard those edits first is a product decision.
        if let Err(e) = self.refuse_dirty_overlap(&tmp) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
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

    fn result(diff: &str, outcome: TestOutcome) -> Result<CandidateResult, String> {
        Ok(CandidateResult {
            diff: diff.into(),
            outcome,
            tampered: Vec::new(),
            tests_only: false,
        })
    }
    fn pass(diff: &str) -> Result<CandidateResult, String> {
        result(diff, TestOutcome::Passed)
    }
    fn fail(diff: &str) -> Result<CandidateResult, String> {
        result(diff, TestOutcome::Failed)
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
                result("+fix", TestOutcome::Unknown),
                result("+fix", TestOutcome::Unknown),
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
    fn a_tests_only_candidate_is_not_a_winner() {
        let tests_only = Ok(CandidateResult {
            tests_only: true,
            ..pass("+t").unwrap()
        });
        let mut a = MockArena::new(vec![tests_only, fail("+real-fix-attempt")]);
        assert_eq!(
            run_best_of_n(&mut a, 2, hard()),
            BestOfNOutcome::NoWinner { attempted: 2 }
        );
        assert!(a.applied.is_none());
    }

    #[test]
    fn binary_index_lines_tell_two_binary_changes_apart() {
        let bin = |h: &str| {
            format!(
                "diff --git a/x.png b/x.png\nindex 1..{h} 100644\nBinary files a/x.png and b/x.png differ\n"
            )
        };
        assert_ne!(normalize_diff(&bin("2")), normalize_diff(&bin("3")));
        let text = |h: &str| format!("diff --git a/x b/x\nindex 1..{h} 100644\n--- a/x\n+++ b/x\n+y\n");
        assert_eq!(normalize_diff(&text("2")), normalize_diff(&text("3")));
    }

    /// C5: the outcome reaches the user once, on the event channel: what was
    /// applied (or why nothing was), tampering candidates with up to five
    /// paths each, and tests-only candidates.
    #[test]
    fn the_outcome_reaches_the_user_once_on_the_event_channel() {
        use crate::protocol::StreamEvent;
        let note_of = |arena: &mut MockArena, n: u32| {
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            let out = run_best_of_n_reported(arena, n, hard(), Some(&tx));
            let events: Vec<StreamEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
            let notes: Vec<String> = events
                .iter()
                .filter_map(|e| match e {
                    StreamEvent::TextDelta { text } => Some(text.clone()),
                    _ => None,
                })
                .collect();
            assert_eq!(events.len(), 1, "{events:?}");
            assert_eq!(notes.len(), 1, "{notes:?}");
            (out, notes[0].clone())
        };
        let tampered = Ok(CandidateResult {
            tampered: (1..=7).map(|i| format!("tests/t{i}.py")).collect(),
            ..fail("+a").unwrap()
        });
        let tests_only = Ok(CandidateResult {
            tests_only: true,
            ..pass("+t").unwrap()
        });
        let mut a = MockArena::new(vec![tampered, tests_only, pass("+fix")]);
        let (out, note) = note_of(&mut a, 3);
        assert!(
            matches!(out, BestOfNOutcome::Applied { winner: 2, .. }),
            "{out:?}"
        );
        assert!(
            note.contains("[origin] Best-of-N applied candidate 3 of 3"),
            "{note}"
        );
        assert!(note.contains("passed the tests"), "{note}");
        assert!(
            note.contains(
                "Candidate 1 changed pre-existing tests: tests/t1.py, tests/t2.py, tests/t3.py, \
                 tests/t4.py, tests/t5.py (+2 more)"
            ),
            "{note}"
        );
        assert!(
            note.contains("tests-only") && note.contains("candidate 2"),
            "{note}"
        );

        let mut b = MockArena::new(vec![fail("x"), fail("y")]);
        let (_, note) = note_of(&mut b, 2);
        assert!(note.contains("none of 2 candidates was applied"), "{note}");
        assert!(note.contains("no candidate passed the tests"), "{note}");

        let mut c = MockArena::new(vec![pass("good"), fail("x")]);
        c.apply_fails = true;
        let (_, note) = note_of(&mut c, 2);
        assert!(note.contains("apply failed"), "names why: {note}");
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

    /// C5: the winner's diff is against HEAD, but the first attempt's edits
    /// are still uncommitted in the user's tree. A diff touching a file the
    /// first attempt changed is refused with a clear reason — git would fail
    /// on the same lines, or silently mix the two edits in one file — and the
    /// file is left as it was.
    #[test]
    fn a_winner_touching_a_file_the_first_attempt_changed_is_not_applied() {
        let lines = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\nline11\nline12\n";
        for (cand_from, cand_to) in [("line2", "CAND2"), ("line11", "CAND11")] {
            let repo = tempfile::tempdir().unwrap();
            let git = crate::selfdev::ProcessGitRunner::new(repo.path());
            git.run(&["init", "-q"]).unwrap();
            std::fs::write(repo.path().join("a.txt"), lines).unwrap();
            git.run(&["add", "-A"]).unwrap();
            git.run(&[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "base",
            ])
            .unwrap();
            let base = git.run(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
            // The first attempt's uncommitted edit.
            let dirty = lines.replace("line2", "SINGLE2");
            std::fs::write(repo.path().join("a.txt"), &dirty).unwrap();
            let scratch = tempfile::tempdir().unwrap();
            let runner = move |_i: usize, wt: &std::path::Path| -> Result<(), String> {
                let p = wt.join("a.txt");
                let s = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
                std::fs::write(&p, s.replace(cand_from, cand_to)).map_err(|e| e.to_string())
            };
            let mut arena =
                WorktreeArena::new(&git, base, scratch.path(), repo.path(), None, Box::new(runner));
            let res = arena.run_candidate(0).unwrap();
            let applied = arena.apply_winner(&res.diff);
            drop(arena);
            let err = applied.expect_err(cand_to);
            assert!(
                err.contains("uncommitted changes") && err.contains("a.txt"),
                "{cand_to}: {err}"
            );
            let now = std::fs::read_to_string(repo.path().join("a.txt")).unwrap();
            assert_eq!(
                now, dirty,
                "{cand_to}: the file is left as the first attempt left it"
            );
        }
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
