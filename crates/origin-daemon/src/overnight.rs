// SPDX-License-Identifier: Apache-2.0
//! Default-off overnight autonomous driver (jcode Overnight mode).
//!
//! When `ORIGIN_OVERNIGHT=1`, the daemon spawns a single **windowed** session
//! that runs an [`OvernightPlan`](origin_ambient::OvernightPlan) to completion
//! within a hard wall-clock ceiling. Each due task's standing prompt is
//! dispatched onto the live agent path (shared self-IPC via
//! [`crate::scheduler::dispatch_prompt`]); outcomes accumulate into a
//! [`MorningReport`](origin_ambient::MorningReport) that is persisted to
//! `~/.origin/overnight/latest.{json,md}` for `origin ambient report` to surface
//! the next morning.
//!
//! Per-task token accounting uses **real usage**: each task is dispatched via
//! the sibling [`crate::scheduler::dispatch_prompt_with_usage`], which drains
//! the turn's [`StreamEvent::Usage`](crate::protocol::StreamEvent) frames and
//! returns their summed `input_tokens + output_tokens`. The
//! [`OvernightDriver`] records a `(task, tokens)` pair per dispatch and the
//! morning report renders a per-task line that also **labels the PR-gated
//! isolation branch** ([`branch_name`](origin_ambient::branch_name)) the task
//! would land on, e.g. `"tests — N tokens (branch origin/ambient/tests-20234)"`.
//! The estimate [`TASK_COST_TOKENS`] is now only a fallback: when the daemon
//! surfaces no `Usage` event the dispatch returns `0`, which [`real_or_estimate`]
//! maps to `None` so [`observe_task_tokens`] substitutes the estimate.
//!
//! Default-off: with the env var unset nothing is spawned, so daemon behaviour
//! is byte-identical. *Closes: jcode Overnight (the windowed driver +
//! morning-report wire + per-task accounting struct + branch labelling + real
//! per-task usage). Remaining follow-up: actually creating/running on a worktree
//! for the labelled branch (a deeper, riskier change that must never switch the
//! user's working tree).*

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use origin_ambient::{AmbientTask, MorningReport, OvernightDriver, OvernightPlan};

/// Default wall-clock ceiling for an overnight session (8 hours).
const DEFAULT_WALL_MS: u64 = 8 * 60 * 60 * 1000;

/// Pause between task dispatches so an overnight run paces itself rather than
/// hammering the agent path back-to-back.
const POLL: Duration = Duration::from_secs(60);

/// Estimated token cost charged per completed task, used as a fallback until
/// `dispatch_prompt` surfaces real usage (see [`observe_task_tokens`]).
const TASK_COST_TOKENS: u64 = 50_000;

/// Per-task token cost to record for an overnight dispatch.
///
/// The single seam for real per-task accounting:
/// [`crate::scheduler::dispatch_prompt_with_usage`] now drains the turn's
/// [`StreamEvent::Usage`](crate::protocol::StreamEvent) frames and returns their
/// summed `input_tokens + output_tokens`, which the loop maps through
/// [`real_or_estimate`] into the `Option` passed here. `Some` records the real
/// figure verbatim; `None` falls back to the [`TASK_COST_TOKENS`] estimate (used
/// when the daemon surfaced no usage). The [`OvernightDriver`] already stores a
/// per-task `(task, tokens)` pair, so the report needs no further change.
#[allow(
    clippy::manual_unwrap_or,
    reason = "Option::unwrap_or is not const-stable on the 1.83 MSRV; the \
              explicit match keeps this fn const so it stays a zero-cost seam"
)]
const fn observe_task_tokens(observed: Option<u64>) -> u64 {
    match observed {
        Some(tokens) => tokens,
        None => TASK_COST_TOKENS,
    }
}

/// Map a dispatch's drained token total into the `observe_task_tokens` input.
///
/// A non-zero total is real usage (`Some`); a `0` total means the daemon emitted
/// no [`StreamEvent::Usage`](crate::protocol::StreamEvent), so we return `None`
/// to keep the [`TASK_COST_TOKENS`] estimate as a fallback.
const fn real_or_estimate(tokens: u64) -> Option<u64> {
    if tokens == 0 {
        None
    } else {
        Some(tokens)
    }
}

/// Spawn the overnight driver if `ORIGIN_OVERNIGHT=1`.
///
/// `sock_path` is the daemon's own IPC socket; dispatched tasks connect back to
/// it as ordinary clients. Default-off: returns immediately when the env var is
/// unset. The spawned task is fire-and-forget (its handle is dropped).
pub fn maybe_spawn(sock_path: String) {
    if std::env::var("ORIGIN_OVERNIGHT").as_deref() != Ok("1") {
        return;
    }
    tracing::info!("overnight: ORIGIN_OVERNIGHT=1 — starting windowed overnight driver");
    tokio::spawn(async move {
        run(sock_path).await;
    });
}

/// Drive the plan to completion within its wall-clock window, then persist the
/// morning report.
async fn run(sock_path: String) {
    let model = std::env::var("ORIGIN_MODEL").unwrap_or_else(|_| "claude-opus-4-7".to_string());
    let wall_ms = std::env::var("ORIGIN_OVERNIGHT_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_WALL_MS);
    let plan = OvernightPlan::new(
        vec![
            AmbientTask::Tests,
            AmbientTask::Refactor,
            AmbientTask::Docs,
            AmbientTask::MemoryGarden,
        ],
        wall_ms,
    );
    let start = now_ms();
    let day = start / 86_400_000;
    let mut driver = OvernightDriver::new(plan, start);

    // gap 6: opt-in per-task git-worktree isolation. Default-off keeps the loop
    // byte-identical (no worktree, no prompt change). When `ORIGIN_OVERNIGHT_WORKTREE=1`
    // each task runs against a dedicated worktree+branch rooted at the workspace.
    let worktree_enabled = std::env::var("ORIGIN_OVERNIGHT_WORKTREE").as_deref() == Ok("1");
    let workspace_root = std::env::var_os("ORIGIN_WORKSPACE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let mut worktrees: Vec<String> = Vec::new();

    loop {
        let now = now_ms();
        if driver.is_finished(now) {
            break;
        }
        let Some(task) = driver.next_due(now) else {
            break;
        };
        let session_id = format!("overnight-{}", task.slug());
        tracing::info!(?task, "overnight: dispatching task");

        // Create the isolation worktree (best-effort) and, on success, point the
        // task prompt at it so the agent edits there rather than the live tree.
        let isolation: Option<PathBuf> = if worktree_enabled {
            overnight_dir().map(|d| d.join("worktrees").join(format!("{day}-{}", task.slug())))
        } else {
            None
        };
        let mut prompt = task_prompt(task).to_string();
        if let Some(ref wt) = isolation {
            let branch = origin_ambient::branch_name(task, day);
            if create_worktree(&workspace_root, wt, &branch) {
                worktrees.push(wt.to_string_lossy().into_owned());
                prompt = isolation_prompt(&prompt, wt, &branch);
            }
        }

        match crate::scheduler::dispatch_prompt_with_usage(&sock_path, &model, session_id, &prompt).await {
            // Real per-turn usage drained from the dispatch reply. A non-zero
            // total is recorded verbatim; a zero total means the daemon emitted
            // no `Usage` event, so we fall back to the estimate via `None`.
            Ok(tokens) => driver.record(task, observe_task_tokens(real_or_estimate(tokens)), None),
            Err(e) => {
                tracing::warn!(?task, error = %e, "overnight: task dispatch failed");
                // Still advance the cursor so a persistently failing task does
                // not wedge the window; record zero tokens for it (it did not run).
                driver.record(task, observe_task_tokens(Some(0)), None);
            }
        }
        tokio::time::sleep(POLL).await;
    }

    // Tear down the run's worktrees (the branches + their commits persist) and
    // prune any stale registrations.
    if worktree_enabled && !worktrees.is_empty() {
        cleanup_worktrees(&workspace_root, &worktrees);
    }

    // `start` is epoch-ms; `day` is the day index used for branch names.
    let report = driver.into_report(day).with_worktrees(worktrees);
    match persist_report(&report) {
        Ok(()) => tracing::info!("overnight: morning report persisted to ~/.origin/overnight/"),
        Err(e) => tracing::warn!(error = %e, "overnight: failed to persist morning report"),
    }
}

/// Append the isolation instruction onto a task's base prompt so the overnight
/// agent edits inside the dedicated worktree rather than the live working tree.
fn isolation_prompt(base: &str, worktree: &Path, branch: &str) -> String {
    format!(
        "{base}\n\nWork ONLY inside the isolated git worktree at {} (branch `{branch}`), \
         so the main working tree stays untouched.",
        worktree.display()
    )
}

/// Create an isolation worktree at `path` on `branch`, rooted at `workspace`.
/// Best-effort: returns `true` on success; on any failure it logs and returns
/// `false` so the task still runs (without isolation).
fn create_worktree(workspace: &Path, path: &Path, branch: &str) -> bool {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(error = %e, "overnight: worktree parent dir create failed");
            return false;
        }
    }
    let runner = crate::selfdev::ProcessGitRunner::new(workspace);
    match origin_vcs::Worktree::new(&runner).add(path, branch) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(error = %e, branch, "overnight: git worktree add failed; running without isolation");
            false
        }
    }
}

/// Remove the run's worktrees and prune stale registrations. Branches (and the
/// commits made in them) are preserved; only the checkouts are removed.
fn cleanup_worktrees(workspace: &Path, paths: &[String]) {
    let runner = crate::selfdev::ProcessGitRunner::new(workspace);
    let wt = origin_vcs::Worktree::new(&runner);
    for p in paths {
        let _ = wt.remove(Path::new(p), true);
    }
    let _ = wt.prune();
}

/// Standing prompt for each overnight task kind (mirrors the ambient loop's set).
const fn task_prompt(task: AmbientTask) -> &'static str {
    match task {
        AmbientTask::Tests => {
            "Run the project's test suite. If any tests fail, investigate the root \
             cause and apply a minimal, focused fix on a dedicated branch. Do not \
             make unrelated changes."
        }
        AmbientTask::Refactor => {
            "Find one small, safe, self-contained refactor that improves readability \
             without changing behavior, and apply it on a dedicated branch."
        }
        AmbientTask::Docs => {
            "Find one public API or module that is undocumented or unclear, and write \
             concise, accurate documentation for it."
        }
        AmbientTask::MemoryGarden => {
            "Review recent session memory for redundant, stale, or contradictory \
             entries and consolidate or prune them. Be conservative."
        }
    }
}

/// `~/.origin/overnight`, honoring `ORIGIN_HOME` (used by tests + the CLI).
pub(crate) fn overnight_dir() -> Option<PathBuf> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)?;
    Some(home.join(".origin").join("overnight"))
}

/// Persist the report as both machine-readable JSON and human Markdown.
fn persist_report(report: &MorningReport) -> std::io::Result<()> {
    let Some(dir) = overnight_dir() else {
        return Ok(());
    };
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(report).map_err(std::io::Error::other)?;
    std::fs::write(dir.join("latest.json"), json)?;
    std::fs::write(dir.join("latest.md"), report.to_markdown())?;
    Ok(())
}

/// Current wall-clock time in milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::{
        isolation_prompt, observe_task_tokens, real_or_estimate, task_prompt, AmbientTask, TASK_COST_TOKENS,
    };
    use origin_ambient::{OvernightDriver, OvernightPlan};

    #[test]
    fn isolation_prompt_appends_worktree_and_branch_after_base() {
        let base = task_prompt(AmbientTask::Tests);
        let out = isolation_prompt(
            base,
            std::path::Path::new("/wt/0-tests"),
            "origin/ambient/tests-0",
        );
        // The base task instruction is preserved as a prefix; the isolation
        // instruction (path + branch) is appended.
        assert!(out.starts_with(base));
        assert!(out.contains("/wt/0-tests"));
        assert!(out.contains("origin/ambient/tests-0"));
        assert!(out.contains("isolated git worktree"));
    }

    #[test]
    fn real_or_estimate_maps_zero_to_none_and_nonzero_to_some() {
        // A drained total of 0 means the daemon emitted no Usage event -> keep
        // the estimate via None; any positive total is real usage via Some.
        assert_eq!(real_or_estimate(0), None);
        assert_eq!(real_or_estimate(1), Some(1));
        assert_eq!(real_or_estimate(123_456), Some(123_456));
    }

    #[test]
    fn dispatch_total_drives_observe_task_tokens() {
        // The loop pipes dispatch_prompt_with_usage's u64 through
        // real_or_estimate into observe_task_tokens: real usage is recorded
        // verbatim; a zero total falls back to the estimate.
        assert_eq!(observe_task_tokens(real_or_estimate(9_001)), 9_001);
        assert_eq!(observe_task_tokens(real_or_estimate(0)), TASK_COST_TOKENS);
    }

    #[test]
    fn every_overnight_task_has_a_nonempty_prompt() {
        for task in [
            AmbientTask::Tests,
            AmbientTask::Refactor,
            AmbientTask::Docs,
            AmbientTask::MemoryGarden,
        ] {
            assert!(!task_prompt(task).is_empty());
        }
    }

    #[test]
    fn observe_task_tokens_prefers_real_usage_else_estimate() {
        // A real observed figure is recorded verbatim.
        assert_eq!(observe_task_tokens(Some(1_234)), 1_234);
        // A failed task observes zero tokens (it did not run).
        assert_eq!(observe_task_tokens(Some(0)), 0);
        // No usage signal -> fall back to the estimate.
        assert_eq!(observe_task_tokens(None), TASK_COST_TOKENS);
    }

    #[test]
    fn per_task_accounting_round_trips_with_branch_labels() {
        // Drive a plan exactly as the loop does, mixing a real-usage figure,
        // the estimate, and a failed (zero-token) task, then assert the report
        // carries per-task tokens AND the PR-gated isolation branch label.
        let plan = OvernightPlan::new(
            vec![AmbientTask::Tests, AmbientTask::Docs, AmbientTask::Refactor],
            1_000_000,
        );
        let mut driver = OvernightDriver::new(plan, 0);
        driver.record(AmbientTask::Tests, observe_task_tokens(Some(7_000)), None);
        driver.record(AmbientTask::Docs, observe_task_tokens(None), None);
        driver.record(AmbientTask::Refactor, observe_task_tokens(Some(0)), None);

        let report = driver.into_report(20_234);

        // Per-task accounting summed across tasks (real + estimate + zero).
        assert_eq!(report.tokens_spent, 7_000 + TASK_COST_TOKENS);
        assert_eq!(report.ran.len(), 3);
        // Each line labels the branch the task would run on (no working-tree
        // switch performed — labelling only).
        assert_eq!(
            report.ran[0],
            "tests — 7000 tokens (branch origin/ambient/tests-20234)"
        );
        assert!(report.ran[1].contains("origin/ambient/docs-20234"));
        assert!(report.ran[2].starts_with("refactor — 0 tokens"));
        assert!(report.ran[2].contains("origin/ambient/refactor-20234"));
    }
}
