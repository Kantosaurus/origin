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
//! Default-off: with the env var unset nothing is spawned, so daemon behaviour
//! is byte-identical. *Closes: jcode Overnight (the windowed driver +
//! morning-report wire). Remaining polish: running each task on a PR-gated
//! isolation branch rather than the working tree, and real per-task token
//! accounting (the estimate below stands in until `dispatch_prompt` surfaces
//! usage).*

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use origin_ambient::{AmbientTask, MorningReport, OvernightDriver, OvernightPlan};

/// Default wall-clock ceiling for an overnight session (8 hours).
const DEFAULT_WALL_MS: u64 = 8 * 60 * 60 * 1000;

/// Pause between task dispatches so an overnight run paces itself rather than
/// hammering the agent path back-to-back.
const POLL: Duration = Duration::from_secs(60);

/// Estimated token cost charged per completed task (until `dispatch_prompt`
/// surfaces real usage).
const TASK_COST_TOKENS: u64 = 50_000;

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
    let mut driver = OvernightDriver::new(plan, start);

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
        match crate::scheduler::dispatch_prompt(&sock_path, &model, session_id, task_prompt(task)).await {
            Ok(()) => driver.record(task, TASK_COST_TOKENS, None),
            Err(e) => {
                tracing::warn!(?task, error = %e, "overnight: task dispatch failed");
                // Still advance the cursor so a persistently failing task does
                // not wedge the window; record zero tokens for it.
                driver.record(task, 0, None);
            }
        }
        tokio::time::sleep(POLL).await;
    }

    // `start` is epoch-ms; `/ 86_400_000` is the day index used for branch names.
    let report = driver.into_report(start / 86_400_000);
    match persist_report(&report) {
        Ok(()) => tracing::info!("overnight: morning report persisted to ~/.origin/overnight/"),
        Err(e) => tracing::warn!(error = %e, "overnight: failed to persist morning report"),
    }
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
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{task_prompt, AmbientTask};

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
}
