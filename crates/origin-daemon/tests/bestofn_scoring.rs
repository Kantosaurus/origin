// SPDX-License-Identifier: Apache-2.0
//! C5: best-of-N scores each candidate on the tests that existed before the
//! run (restoring ones it changed, setting aside ones it added), flags a
//! candidate that changed a pre-existing test, scores identical diffs once and
//! stops early when the first two agree and pass. A6: the winner's `git apply`
//! child gets the scrubbed environment. Real git in temp directories.

#![allow(clippy::panic, clippy::unwrap_used)]

use origin_daemon::bestofn_runner::{run_best_of_n, BestOfNOutcome, CandidateArena, WorktreeArena};
use origin_daemon::selfdev::ProcessGitRunner;
use origin_swarm::bestofn::{DifficultySignals, TestOutcome};
use origin_vcs::GitRunner as _;
use std::path::Path;
use std::sync::Mutex;

// One test sets GIT_TRACE process-wide; the others spawn git too.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// A repo whose pre-existing test `tests/expect.txt` says `lib.txt` must read
/// `fixed`; it starts as `bug`. Returns (repo dir, base commit).
fn repo() -> (tempfile::TempDir, String) {
    repo_with("bug\n")
}

/// [`repo`] with `lib.txt` starting as `lib`.
fn repo_with(lib: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let git = ProcessGitRunner::new(dir.path());
    git.run(&["init", "-q"]).unwrap();
    git.run(&["config", "core.autocrlf", "false"]).unwrap();
    std::fs::create_dir(dir.path().join("tests")).unwrap();
    std::fs::write(dir.path().join("lib.txt"), lib).unwrap();
    std::fs::write(dir.path().join("tests/expect.txt"), "fixed\n").unwrap();
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
    (dir, base)
}

/// Passes iff `lib.txt` matches `tests/expect.txt` and no `tests/veto.txt`
/// exists; every run appends a line to `count`.
fn test_command(count: &Path) -> String {
    let count = count.to_str().unwrap();
    #[cfg(unix)]
    return format!(
        "echo x >> '{count}'; if [ -e tests/veto.txt ]; then exit 1; fi; cmp -s lib.txt tests/expect.txt"
    );
    #[cfg(windows)]
    return format!(
        "Add-Content '{count}' x; if ((Test-Path tests/veto.txt) -or ((Get-Content lib.txt) -ne (Get-Content \
         tests/expect.txt))) {{ exit 1 }}"
    );
}

fn runs(count: &Path) -> usize {
    std::fs::read_to_string(count).map_or(0, |s| s.lines().count())
}

type Edit = (&'static str, &'static str);

/// Content that makes the scripted agent delete the path instead.
const DELETE: &str = "<delete>";

/// Run candidate `index` whose agent writes `edits` (path, content).
fn arena<'a>(
    git: &'a ProcessGitRunner,
    base: &str,
    scratch: &Path,
    repo: &Path,
    count: &Path,
    edits: Vec<Vec<Edit>>,
    calls: &'a std::cell::Cell<usize>,
) -> WorktreeArena<'a> {
    let runner = move |i: usize, wt: &Path| -> Result<(), String> {
        calls.set(calls.get() + 1);
        for (p, c) in &edits[i] {
            let done = if *c == DELETE {
                std::fs::remove_file(wt.join(p))
            } else {
                std::fs::write(wt.join(p), c)
            };
            done.map_err(|e| e.to_string())?;
        }
        Ok(())
    };
    WorktreeArena::new(
        git,
        base,
        scratch,
        repo,
        Some(test_command(count)),
        Box::new(runner),
    )
}

#[test]
fn a_candidate_that_weakens_a_pre_existing_test_is_scored_on_the_original_and_flagged() {
    let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (dir, base) = repo();
    let scratch = tempfile::tempdir().unwrap();
    let count = scratch.path().join("count");
    let git = ProcessGitRunner::new(dir.path());
    let calls = std::cell::Cell::new(0);
    let mut a = arena(
        &git,
        &base,
        scratch.path(),
        dir.path(),
        &count,
        vec![vec![("lib.txt", "wrong\n"), ("tests/expect.txt", "wrong\n")]],
        &calls,
    );
    let res = a.run_candidate(0).unwrap();
    assert_eq!(res.outcome, TestOutcome::Failed, "scored on the ORIGINAL test");
    assert_eq!(res.tampered, ["tests/expect.txt"], "the weakened test is flagged");
    assert!(
        res.diff.contains("tests/expect.txt"),
        "the diff is unchanged: {}",
        res.diff
    );
    let wt = scratch.path().join("origin-bestofn-0");
    assert_eq!(
        std::fs::read_to_string(wt.join("tests/expect.txt")).unwrap(),
        "wrong\n",
        "the candidate's version is put back"
    );
}

#[test]
fn a_test_the_candidate_added_does_not_affect_its_score() {
    let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (dir, base) = repo();
    let scratch = tempfile::tempdir().unwrap();
    let count = scratch.path().join("count");
    let git = ProcessGitRunner::new(dir.path());
    let calls = std::cell::Cell::new(0);
    let mut a = arena(
        &git,
        &base,
        scratch.path(),
        dir.path(),
        &count,
        vec![vec![("lib.txt", "fixed\n"), ("tests/veto.txt", "veto\n")]],
        &calls,
    );
    let res = a.run_candidate(0).unwrap();
    assert_eq!(
        res.outcome,
        TestOutcome::Passed,
        "the added veto test is set aside"
    );
    assert!(res.tampered.is_empty(), "adding a test is not tampering");
    let wt = scratch.path().join("origin-bestofn-0");
    assert!(wt.join("tests/veto.txt").exists(), "the added test is put back");
}

#[test]
fn identical_candidates_are_scored_once_and_stop_the_run_early() {
    let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (dir, base) = repo();
    let scratch = tempfile::tempdir().unwrap();
    let count = scratch.path().join("count");
    let git = ProcessGitRunner::new(dir.path());
    let calls = std::cell::Cell::new(0);
    let fix = vec![("lib.txt", "fixed\n")];
    let mut a = arena(
        &git,
        &base,
        scratch.path(),
        dir.path(),
        &count,
        vec![fix.clone(), fix, vec![("lib.txt", "other\n")]],
        &calls,
    );
    let hard = DifficultySignals {
        tests_red: true,
        ..Default::default()
    };
    let out = run_best_of_n(&mut a, 3, hard);
    drop(a);
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
    assert_eq!(calls.get(), 2, "the third candidate is not run");
    assert_eq!(runs(&count), 1, "the identical diff is scored once");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("lib.txt")).unwrap(),
        "fixed\n"
    );
}

#[test]
fn different_candidates_are_each_scored() {
    let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (dir, base) = repo();
    let scratch = tempfile::tempdir().unwrap();
    let count = scratch.path().join("count");
    let git = ProcessGitRunner::new(dir.path());
    let calls = std::cell::Cell::new(0);
    let mut a = arena(
        &git,
        &base,
        scratch.path(),
        dir.path(),
        &count,
        vec![vec![("lib.txt", "fixed\n")], vec![("lib.txt", "other\n")]],
        &calls,
    );
    assert_eq!(a.run_candidate(0).unwrap().outcome, TestOutcome::Passed);
    assert_eq!(a.run_candidate(1).unwrap().outcome, TestOutcome::Failed);
    assert_eq!(runs(&count), 2);
}

#[test]
fn a_candidate_that_changes_only_tests_does_not_win() {
    let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // A green base: a tests-only change "passes" without fixing anything.
    let (dir, base) = repo_with("fixed\n");
    let scratch = tempfile::tempdir().unwrap();
    let count = scratch.path().join("count");
    let git = ProcessGitRunner::new(dir.path());
    let calls = std::cell::Cell::new(0);
    let mut a = arena(
        &git,
        &base,
        scratch.path(),
        dir.path(),
        &count,
        vec![vec![("tests/extra.txt", "more\n")], vec![("lib.txt", "other\n")]],
        &calls,
    );
    let res = a.run_candidate(0).unwrap();
    assert_eq!(res.outcome, TestOutcome::Passed);
    assert!(res.tests_only, "its result says it changed only tests");
    let hard = DifficultySignals {
        tests_red: true,
        ..Default::default()
    };
    let scratch2 = tempfile::tempdir().unwrap();
    let mut a = arena(
        &git,
        &base,
        scratch2.path(),
        dir.path(),
        &count,
        vec![vec![("tests/extra.txt", "more\n")], vec![("lib.txt", "other\n")]],
        &calls,
    );
    assert_eq!(
        run_best_of_n(&mut a, 2, hard),
        BestOfNOutcome::NoWinner { attempted: 2 }
    );
}

#[test]
fn a_change_list_that_cannot_be_read_scores_unknown() {
    let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (dir, base) = repo_with("fixed\n");
    let scratch = tempfile::tempdir().unwrap();
    let count = scratch.path().join("count");
    let git = ProcessGitRunner::new(dir.path());
    let calls = std::cell::Cell::new(0);
    // A broken `.git` link makes every git call in the worktree fail (without
    // git walking up to some enclosing repository).
    let mut a = arena(
        &git,
        &base,
        scratch.path(),
        dir.path(),
        &count,
        vec![vec![
            ("tests/expect.txt", "weakened\n"),
            (".git", "gitdir: no-such-dir\n"),
        ]],
        &calls,
    );
    let res = a.run_candidate(0).unwrap();
    assert_eq!(res.outcome, TestOutcome::Unknown, "never scored on its own tests");
    assert_eq!(runs(&count), 0);
}

#[test]
fn a_deleted_pre_existing_test_is_restored_for_scoring_then_deleted_again() {
    let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (dir, base) = repo();
    let scratch = tempfile::tempdir().unwrap();
    let count = scratch.path().join("count");
    let git = ProcessGitRunner::new(dir.path());
    let calls = std::cell::Cell::new(0);
    let mut a = arena(
        &git,
        &base,
        scratch.path(),
        dir.path(),
        &count,
        vec![vec![("lib.txt", "wrong\n"), ("tests/expect.txt", DELETE)]],
        &calls,
    );
    let res = a.run_candidate(0).unwrap();
    assert_eq!(res.outcome, TestOutcome::Failed, "scored on the restored test");
    assert_eq!(res.tampered, ["tests/expect.txt"]);
    let wt = scratch.path().join("origin-bestofn-0");
    assert!(!wt.join("tests/expect.txt").exists(), "the deletion is put back");
}

/// A6: `git apply` honours `GIT_TRACE`, which the allow-list drops; an
/// inherited one would make it write the trace file.
#[test]
fn apply_winner_runs_git_with_the_scrubbed_environment() {
    let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (dir, base) = repo();
    let scratch = tempfile::tempdir().unwrap();
    let count = scratch.path().join("count");
    let git = ProcessGitRunner::new(dir.path());
    let calls = std::cell::Cell::new(0);
    let mut a = arena(&git, &base, scratch.path(), dir.path(), &count, vec![], &calls);
    let patch = "--- a/lib.txt\n+++ b/lib.txt\n@@ -1 +1 @@\n-bug\n+fixed\n";
    let trace = scratch.path().join("trace.log");
    std::env::remove_var("ORIGIN_ENV_SCRUB");
    std::env::set_var("GIT_TRACE", &trace);
    let applied = a.apply_winner(patch);
    std::env::remove_var("GIT_TRACE");
    applied.unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("lib.txt")).unwrap(),
        "fixed\n"
    );
    assert!(!trace.exists(), "git apply inherited GIT_TRACE");
}
