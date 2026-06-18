// SPDX-License-Identifier: Apache-2.0
//! Daemon-wide registry of DETACHED (background) swarm sub-agents.
//!
//! When a `Task` is dispatched with `background: true` (the default), the turn
//! registers the worker here instead of awaiting it (see `agent.rs`), then
//! returns immediately — the worker keeps running on the coordinator's pool
//! (detach-on-drop). Finished results are reaped non-blockingly and delivered to
//! the model at the start of a later turn (a `<background-results>` block) or on
//! demand via the `CollectTasks` tool. This is what keeps the main agent
//! responsive while sub-agents run.

use std::collections::HashMap;
use std::time::Instant;

use origin_swarm::{Coordinator, WorkerHandle};
use origin_tools::builtins::task::{task_try_collect, TaskOutput};
use tokio::sync::Mutex;

/// Per-session cap on outstanding background workers. Past this, a new `Task`
/// falls back to a blocking in-turn await (never silently dropped). Override
/// with `ORIGIN_SWARM_BG_MAX` (default 32).
fn session_cap() -> usize {
    std::env::var("ORIGIN_SWARM_BG_MAX")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(32)
}

/// One registered detached worker (its coordinator handle + display metadata).
#[derive(Debug, Clone)]
pub struct BgJob {
    /// Stable hex worker id (matches the `SwarmWorker`/`SwarmAgentOutput` panel id).
    pub id_hex: String,
    /// The sub-agent's goal (for the delivered summary).
    pub goal: String,
    /// Coordinator handle used to reap the worker (Copy lookup key).
    pub handle: WorkerHandle,
    /// When the worker was dispatched (for diagnostics / future TTLs).
    pub dispatched_at: Instant,
}

/// A reaped terminal background result, rendered into the `<background-results>`
/// block and returned by `CollectTasks`.
#[derive(Debug, Clone)]
pub struct BgResult {
    /// Stable hex worker id.
    pub id_hex: String,
    /// The sub-agent's goal.
    pub goal: String,
    /// The worker's actionable output (or a synthetic `aborted` view for a
    /// worker that vanished — e.g. lost to a daemon restart).
    pub output: TaskOutput,
}

/// The session already has the maximum outstanding background workers; the
/// caller must await the new `Task` in-turn instead so work is never dropped.
#[derive(Debug)]
pub struct AtCap;

/// Daemon-wide registry of background jobs, keyed by session id. Lives next to
/// the [`Coordinator`] (both outlive every turn).
#[derive(Default)]
pub struct BackgroundJobs {
    jobs: Mutex<HashMap<String, Vec<BgJob>>>,
}

impl BackgroundJobs {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a detached worker for `session_id`. Returns `Err(AtCap)` once the
    /// session holds [`session_cap`] outstanding jobs — the caller then awaits the
    /// worker in-turn (blocking) so work is never silently dropped.
    ///
    /// # Errors
    /// [`AtCap`] when the per-session outstanding-worker cap is reached.
    // The lock MUST span the cap-check and the push (atomic check-then-insert), so
    // it cannot be tightened further — the lint is a false positive here.
    #[allow(clippy::significant_drop_tightening)]
    pub async fn register(&self, session_id: &str, job: BgJob) -> Result<(), AtCap> {
        let cap = session_cap();
        let mut map = self.jobs.lock().await;
        let v = map.entry(session_id.to_string()).or_default();
        if v.len() >= cap {
            return Err(AtCap);
        }
        v.push(job);
        Ok(())
    }

    /// Number of outstanding (registered, not-yet-reaped) jobs for a session.
    pub async fn count(&self, session_id: &str) -> usize {
        self.jobs.lock().await.get(session_id).map_or(0, Vec::len)
    }

    /// Reap every TERMINAL background job for `session_id`: poll each via
    /// [`task_try_collect`], collect the finished ones, remove them from the
    /// registry, and evict them from the coordinator. Still-running jobs stay
    /// registered. A vanished worker (unknown handle — e.g. lost to a restart)
    /// reaps as a synthetic `aborted` result so the model is never left waiting.
    /// Results are delivered exactly once (removed on drain).
    pub async fn drain_terminal(&self, session_id: &str, coord: &Coordinator) -> Vec<BgResult> {
        // Snapshot the handles, then reap OUTSIDE the registry lock (try_completion
        // takes the coordinator lock); re-lock only to remove the reaped ids.
        let snapshot: Vec<BgJob> = {
            let map = self.jobs.lock().await;
            map.get(session_id).cloned().unwrap_or_default()
        };
        if snapshot.is_empty() {
            return Vec::new();
        }
        let mut done: Vec<BgResult> = Vec::new();
        let mut reaped: Vec<String> = Vec::new();
        for job in snapshot {
            let Some(result) = task_try_collect(coord, &job.handle, &job.goal).await else {
                continue; // still running
            };
            let output = result.unwrap_or_else(|e| TaskOutput {
                status: "aborted".to_string(),
                summary: format!("background worker for {:?} aborted: {e}", job.goal),
                files_touched: Vec::new(),
                follow_ups: Vec::new(),
            });
            coord.evict(&job.handle).await;
            reaped.push(job.id_hex.clone());
            done.push(BgResult {
                id_hex: job.id_hex,
                goal: job.goal,
                output,
            });
        }
        if !reaped.is_empty() {
            let mut map = self.jobs.lock().await;
            if let Some(v) = map.get_mut(session_id) {
                v.retain(|j| !reaped.contains(&j.id_hex));
                if v.is_empty() {
                    map.remove(session_id);
                }
            }
        }
        done
    }

    /// Still-running jobs for a session (id + goal) — `CollectTasks`'s
    /// `still_running` payload.
    pub async fn pending(&self, session_id: &str) -> Vec<(String, String)> {
        self.jobs.lock().await.get(session_id).map_or_else(Vec::new, |v| {
            v.iter().map(|j| (j.id_hex.clone(), j.goal.clone())).collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(n: u128) -> BgJob {
        BgJob {
            id_hex: format!("{n:032x}"),
            goal: format!("goal-{n}"),
            handle: WorkerHandle::from_raw_for_test(n),
            dispatched_at: Instant::now(),
        }
    }

    #[tokio::test]
    async fn register_counts_and_caps_per_session() {
        std::env::set_var("ORIGIN_SWARM_BG_MAX", "2");
        let bg = BackgroundJobs::new();
        assert_eq!(bg.count("s1").await, 0);
        bg.register("s1", job(1)).await.expect("1st");
        bg.register("s1", job(2)).await.expect("2nd");
        assert_eq!(bg.count("s1").await, 2);
        // Third over the cap ⇒ AtCap (caller blocks in-turn instead).
        assert!(bg.register("s1", job(3)).await.is_err(), "cap enforced");
        // A different session has its own budget.
        bg.register("s2", job(4)).await.expect("other session");
        assert_eq!(bg.count("s2").await, 1);
        std::env::remove_var("ORIGIN_SWARM_BG_MAX");
    }

    #[tokio::test]
    async fn pending_lists_still_running_jobs() {
        let bg = BackgroundJobs::new();
        bg.register("s", job(7)).await.expect("reg");
        let p = bg.pending("s").await;
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].1, "goal-7");
    }
}
