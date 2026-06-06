// SPDX-License-Identifier: Apache-2.0
//! Default-off ambient (always-on) autonomy tick loop (item K-daemon).
//!
//! When `ORIGIN_AMBIENT=1` is set, the daemon spawns a background task that
//! mirrors jcode's Ambient/OpenClaw mode: on each tick it consults the
//! resource-aware [`origin_ambient`] policy core to decide — under a token
//! budget that always reserves headroom for the interactive user, **and only
//! once the user has been idle for at least [`IdleTracker`]'s threshold** —
//! whether to run a unit of proactive background work, and which task to pick
//! next. When a task is selected it is **dispatched onto the live agent path**
//! by submitting its prompt to the daemon's own IPC socket (shared with the
//! scheduler loop via [`crate::scheduler::dispatch_prompt`]).
//!
//! The idle gate is fed by an [`IdleTracker`] the loop owns. The prompt path is
//! expected to call [`note_user_activity`] on every interactive turn so the
//! tracker reflects real activity; that one-line wire (in the daemon's
//! prompt-handling path) is the only piece still pending — until it lands the
//! tracker measures idleness from loop start / its own dispatches, which is
//! conservative (it never interrupts the user, it just may run sooner than a
//! fully-wired tracker would after a burst of typing).
//!
//! With the env var unset nothing is spawned, so default daemon behaviour is
//! byte-identical. *Closes: jcode Ambient/OpenClaw + Overnight (the dispatch
//! wire); cline always-on autonomy.*

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use origin_ambient::{next_task, AmbientTask, BudgetPolicy, IdleTracker, DEFAULT_MIN_IDLE_MS};

/// Interval between ambient ticks. Longer than the scheduler tick: ambient work
/// is opportunistic, not deadline-driven.
const TICK: Duration = Duration::from_secs(60);

/// Total per-process ambient token budget (proactive work + user reserve).
const TOTAL_BUDGET_TOKENS: u64 = 1_000_000;

/// Tokens reserved for the interactive user; ambient work never dips below this.
const USER_RESERVE_TOKENS: u64 = 200_000;

/// Estimated token cost of one ambient task, charged against the budget.
const TASK_COST_TOKENS: u64 = 50_000;

/// Process-wide idle tracker shared between the ambient loop (which owns the
/// gate) and the prompt path (which should bump it). Initialised lazily by
/// [`maybe_spawn`] when ambient mode is enabled; remains `None` otherwise so
/// default builds carry no extra state.
static IDLE_TRACKER: std::sync::OnceLock<Arc<IdleTracker>> = std::sync::OnceLock::new();

/// Record interactive user activity for the ambient idle gate, if ambient mode
/// is active.
///
/// Intended to be called once per user turn from the daemon's prompt-handling
/// path (a future one-line wire in `agent.rs`/`main.rs`, out of this module's
/// ownership). A no-op when ambient mode is off (the tracker was never
/// initialised), so it is always safe to call unconditionally.
pub fn note_user_activity() {
    if let Some(tracker) = IDLE_TRACKER.get() {
        tracker.note_activity(now_ms());
    }
}

/// Spawn the background ambient loop if `ORIGIN_AMBIENT=1`.
///
/// `sock_path` is the daemon's own IPC socket/pipe path; selected tasks connect
/// back to it as ordinary clients.
///
/// Default-off: returns immediately (spawning nothing) when the env var is
/// unset or not exactly `"1"`. The spawned task runs for the life of the
/// process; its handle is intentionally dropped (fire-and-forget background
/// work, mirroring [`crate::scheduler::maybe_spawn`]).
pub fn maybe_spawn(sock_path: String) {
    if std::env::var("ORIGIN_AMBIENT").as_deref() != Ok("1") {
        return;
    }
    tracing::info!("ambient: ORIGIN_AMBIENT=1 — starting background tick loop");
    // Publish the shared idle tracker so the prompt path can bump it via
    // `note_user_activity`. Seeded at "now" so the first dispatch waits a full
    // idle window rather than firing on startup.
    let tracker = IDLE_TRACKER
        .get_or_init(|| Arc::new(IdleTracker::new(now_ms())))
        .clone();
    tokio::spawn(async move {
        run_loop(sock_path, tracker).await;
    });
}

/// The tick loop: every [`TICK`], decide whether to run an ambient task — only
/// when the user has been idle long enough *and* the budget policy permits —
/// and, if so, dispatch its prompt onto the live agent path.
async fn run_loop(sock_path: String, idle: Arc<IdleTracker>) {
    let model = std::env::var("ORIGIN_MODEL").unwrap_or_else(|_| "claude-opus-4-7".to_string());
    let min_idle_ms = std::env::var("ORIGIN_AMBIENT_IDLE_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MIN_IDLE_MS);
    let budget = BudgetPolicy::new(TOTAL_BUDGET_TOKENS, USER_RESERVE_TOKENS);
    let mut spent_today: u64 = 0;
    let mut recent: Vec<AmbientTask> = Vec::new();
    loop {
        tokio::time::sleep(TICK).await;
        if !idle.is_idle(now_ms(), min_idle_ms) {
            tracing::debug!("ambient: user not idle long enough — skipping tick");
            continue;
        }
        let Some(task) = select_task(&budget, spent_today, &recent) else {
            continue;
        };
        spent_today = spent_today.saturating_add(TASK_COST_TOKENS);
        recent.push(task);
        if recent.len() > 8 {
            recent.remove(0);
        }
        tracing::info!(?task, "ambient: task selected — dispatching prompt");
        let session_id = format!("ambient-{}", task.slug());
        if let Err(e) =
            crate::scheduler::dispatch_prompt(&sock_path, &model, session_id, task_prompt(task)).await
        {
            tracing::warn!(?task, error = %e, "ambient: dispatch failed");
        }
    }
}

/// One ambient decision: returns the next task to run when the budget policy
/// still has non-reserved headroom for it, or `None` to protect the user
/// reserve. Pure given its inputs, so it is unit-testable without a runtime.
///
/// The idle gate is applied separately in [`run_loop`] (it needs the clock);
/// this stays clock-free for unit testing.
fn select_task(budget: &BudgetPolicy, spent_today: u64, recent: &[AmbientTask]) -> Option<AmbientTask> {
    if budget.may_run(spent_today, TASK_COST_TOKENS) {
        Some(next_task(recent))
    } else {
        None
    }
}

/// Current wall-clock time in milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The standing prompt for each ambient task kind. Each is phrased as a small,
/// safe, self-contained unit of proactive work.
const fn task_prompt(task: AmbientTask) -> &'static str {
    match task {
        AmbientTask::Tests => {
            "Run the project's test suite. If any tests fail, investigate the root \
             cause and apply a minimal, focused fix. Do not make unrelated changes."
        }
        AmbientTask::Refactor => {
            "Find one small, safe, self-contained refactor that improves readability \
             without changing behavior, and apply it. Keep the change minimal."
        }
        AmbientTask::Docs => {
            "Find one public API or module that is undocumented or unclear, and write \
             concise, accurate documentation for it."
        }
        AmbientTask::MemoryGarden => {
            "Review the recent session memory for redundant, stale, or contradictory \
             entries and consolidate or prune them. Be conservative."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        select_task, task_prompt, AmbientTask, BudgetPolicy, IdleTracker, DEFAULT_MIN_IDLE_MS,
        TASK_COST_TOKENS,
    };

    /// Mirror of the `run_loop` gate: dispatch only when the user is idle long
    /// enough *and* the budget policy permits. Kept clock-injected so it is
    /// deterministic without a runtime.
    fn gated_task(
        idle: &IdleTracker,
        now_ms: u64,
        min_idle_ms: u64,
        budget: &BudgetPolicy,
        spent_today: u64,
        recent: &[AmbientTask],
    ) -> Option<AmbientTask> {
        if !idle.is_idle(now_ms, min_idle_ms) {
            return None;
        }
        select_task(budget, spent_today, recent)
    }

    #[test]
    fn idle_gate_blocks_until_threshold_then_budget_decides() {
        let budget = BudgetPolicy::new(1_000_000, 200_000);
        let idle = IdleTracker::new(0);
        // Before the idle window elapses, nothing dispatches even with budget.
        assert!(
            gated_task(&idle, 1_000, DEFAULT_MIN_IDLE_MS, &budget, 0, &[]).is_none(),
            "user still active -> no ambient work"
        );
        // Once idle long enough, the budget policy picks the next task.
        let picked = gated_task(&idle, DEFAULT_MIN_IDLE_MS, DEFAULT_MIN_IDLE_MS, &budget, 0, &[])
            .expect("idle + budget -> task");
        assert_eq!(picked, AmbientTask::Tests);
    }

    #[test]
    fn idle_but_no_budget_still_yields_nothing() {
        // Budget leaves no headroom; even a long-idle user gets no ambient work.
        let budget = BudgetPolicy::new(TASK_COST_TOKENS + 200_000, 200_000);
        let idle = IdleTracker::new(0);
        assert!(
            gated_task(
                &idle,
                10 * DEFAULT_MIN_IDLE_MS,
                DEFAULT_MIN_IDLE_MS,
                &budget,
                TASK_COST_TOKENS,
                &[]
            )
            .is_none(),
            "no non-reserved headroom -> no task even when idle"
        );
    }

    #[test]
    fn activity_resets_the_idle_gate() {
        let budget = BudgetPolicy::new(1_000_000, 200_000);
        let idle = IdleTracker::new(0);
        // Idle long enough at first.
        assert!(gated_task(&idle, DEFAULT_MIN_IDLE_MS, DEFAULT_MIN_IDLE_MS, &budget, 0, &[]).is_some());
        // User acts again -> the gate closes until a fresh window elapses.
        idle.note_activity(DEFAULT_MIN_IDLE_MS);
        assert!(
            gated_task(
                &idle,
                DEFAULT_MIN_IDLE_MS + 1_000,
                DEFAULT_MIN_IDLE_MS,
                &budget,
                0,
                &[]
            )
            .is_none(),
            "fresh activity reopens the idle wait"
        );
    }

    #[test]
    fn schedules_and_cycles_tasks_when_in_budget() {
        let budget = BudgetPolicy::new(1_000_000, 200_000);
        let first = select_task(&budget, 0, &[]).expect("first task scheduled");
        assert_eq!(first, AmbientTask::Tests);
        let second = select_task(&budget, TASK_COST_TOKENS, &[first]).expect("second task scheduled");
        assert_eq!(second, AmbientTask::Refactor, "next_task avoids repeating");
    }

    #[test]
    fn stops_scheduling_when_budget_exhausted() {
        // A budget where the user reserve leaves room for exactly one task.
        let budget = BudgetPolicy::new(TASK_COST_TOKENS + 200_000, 200_000);
        assert!(select_task(&budget, 0, &[]).is_some(), "first fits");
        assert!(
            select_task(&budget, TASK_COST_TOKENS, &[]).is_none(),
            "second exceeds non-reserved headroom and is skipped"
        );
    }

    #[test]
    fn every_task_has_a_nonempty_prompt() {
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
