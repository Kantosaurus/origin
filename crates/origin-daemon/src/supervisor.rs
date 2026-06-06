// SPDX-License-Identifier: Apache-2.0
//! Daemon-side wiring for the pure [`origin_supervisor::LifecyclePolicy`].
//!
//! The supervisor crate ships a deterministic, I/O-free policy that decides
//! when to *shed* (memory pressure), *retire* (idle grace), or *hand off*
//! (attach/detach) agent sessions. This module is the daemon's thin adapter
//! that supplies the real clock, a best-effort RSS sampler, and the live
//! session table the policy folds events into.
//!
//! # Default-off / conservative by construction
//!
//! - The policy is built from [`origin_supervisor::LifecycleConfig::from_env`],
//!   whose defaults (5-min idle grace, 30-min detached grace, 1 GiB budget,
//!   shed at 90%) never touch a normal short interactive session.
//! - The periodic tick only *acts* on a session when its grace lapses or the
//!   daemon is over its memory budget; otherwise it is a `Keep` no-op.
//! - The foreground [`SessionClass::Interactive`] session is never shed and is
//!   only ever retired after the (long) idle grace — and even then, the
//!   *destructive* teardown of a live connection is opt-in via
//!   [`ENFORCE_ENV`]. By default a retire/shed only drops the supervisor's own
//!   in-memory bookkeeping and logs; the real session on disk is untouched.
//! - When the RSS sampler returns `None` (unsupported platform), the mem-shed
//!   path is skipped entirely — idle-retire and attach/detach still run.
//!
//! The whole module is process-global state initialised lazily by
//! [`init`], mirroring [`crate::ambient`]'s `IDLE_TRACKER` pattern.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use origin_resume_token::ResumeToken;
use origin_supervisor::{Decision, LifecycleConfig, LifecyclePolicy, MemReading, SessionClass, SessionState};

/// Opt-in gate for *destructive* enforcement. Unset (the default): a retire or
/// shed decision only drops the supervisor's in-memory tracking and logs — it
/// never kills a live connection or deletes session state. Set to `1`: future
/// destructive teardown may run (none is wired yet; see [`apply_decision`]).
const ENFORCE_ENV: &str = "ORIGIN_SUPERVISOR_ENFORCE";

/// Default tick cadence (ms). Each tick samples RSS, plans+applies shedding,
/// then runs one idle-tick per tracked session. Overridable via
/// `ORIGIN_SUPERVISOR_TICK_MS` (clamped to a sane floor).
const DEFAULT_TICK_MS: u64 = 12_000;

/// Lower bound on the tick cadence so a pathological env value cannot spin.
const MIN_TICK_MS: u64 = 1_000;

/// Process-global supervisor state. `None` until [`init`] runs; a no-op for
/// every call before then so the daemon is byte-identical when uninitialised.
static STATE: OnceLock<SupervisorState> = OnceLock::new();

/// The daemon's live lifecycle state: the immutable policy plus the mutable
/// session table the policy operates on, plus a monotonic clock origin.
struct SupervisorState {
    policy: LifecyclePolicy,
    /// Tracked sessions keyed by session id, snapshotted to a `Vec` for the
    /// policy's slice-based shed planner. A `std::sync::Mutex` (not tokio):
    /// every critical section is a quick, non-await fold.
    sessions: Mutex<HashMap<String, SessionState>>,
    /// Process-start instant; `now_ms` is measured from here so the policy's
    /// monotonic millisecond clock is immune to wall-clock jumps.
    start: Instant,
}

impl SupervisorState {
    /// Monotonic milliseconds since process start (saturating).
    fn now_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, SessionState>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Wall-clock seconds since the Unix epoch (for token `detached_at_unix`).
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `true` iff destructive enforcement is opted into.
fn enforce_enabled() -> bool {
    std::env::var(ENFORCE_ENV).as_deref() == Ok("1")
}

/// Best-effort resident-set-size of THIS process, in bytes.
///
/// Linux reads `/proc/self/statm` (`resident_pages * page_size`) with no extra
/// dependency. Every other platform returns `None`, which makes the tick skip
/// the memory-shed pass entirely (idle-retire / attach-detach still run). This
/// is the "no RSS sampler readily available ⇒ best-effort, clearly commented"
/// fallback the task calls for — we deliberately avoid pulling `sysinfo` into
/// the daemon's runtime dependency set.
#[must_use]
// On non-Linux targets the body is a bare `None`, which clippy would flag as
// const-able; on Linux it does file I/O and cannot be const. Allow uniformly.
#[allow(clippy::missing_const_for_fn)]
pub fn sample_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // Page size is 4096 on every Linux target origin supports; reading it
        // dynamically would need libc (`sysconf`), which we avoid here.
        const PAGE_BYTES: u64 = 4096;
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        // Fields are in pages: size, resident, shared, text, lib, data, dt.
        let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(resident_pages.saturating_mul(PAGE_BYTES))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Initialise the process-global supervisor state and spawn the periodic tick.
///
/// Idempotent: a second call is a no-op (the `OnceLock` is already set). Always
/// safe to call at startup — the conservative defaults mean a normal session is
/// never shed or retired. The spawned tick runs for the life of the process.
pub fn init() {
    if STATE.get().is_some() {
        return;
    }
    let policy = LifecyclePolicy::new(LifecycleConfig::from_env());
    let state = SupervisorState {
        policy,
        sessions: Mutex::new(HashMap::new()),
        start: Instant::now(),
    };
    // Lose the race gracefully: if another thread set it first, use theirs.
    if STATE.set(state).is_err() {
        return;
    }
    let tick_ms = std::env::var("ORIGIN_SUPERVISOR_TICK_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TICK_MS)
        .max(MIN_TICK_MS);
    tracing::info!(
        tick_ms,
        enforce = enforce_enabled(),
        "supervisor lifecycle: tick loop started"
    );
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(tick_ms));
        interval.tick().await; // skip the immediate first tick
        loop {
            interval.tick().await;
            tick();
        }
    });
}

/// Register a session as it opens (or refresh its activity if already tracked).
///
/// `class` is [`SessionClass::Interactive`] for the foreground client session
/// and [`SessionClass::Detached`] for daemon self-dispatch (ambient / scheduler
/// / Task / overnight / webhook) sessions. A no-op when [`init`] never ran.
pub fn register_session(session_id: &str, class: SessionClass) {
    let Some(state) = STATE.get() else { return };
    let now = state.now_ms();
    let mut map = state.lock();
    map.entry(session_id.to_string())
        .and_modify(|s| LifecyclePolicy::on_activity(s, now))
        .or_insert_with(|| SessionState::new(session_id, class, now));
}

/// Record an inbound prompt/turn on a session: refresh its idle clock. Also
/// registers the session if it was not seen before. A no-op when uninitialised.
pub fn note_activity(session_id: &str, class: SessionClass) {
    register_session(session_id, class);
}

/// Move a session to `Detached` on client disconnect (WS-supervisor).
///
/// Returns the annotated [`ResumeToken`] the caller should persist (keeping the
/// process alive). The `base_token` is the daemon's current checkpoint for the
/// session. Returns `None` when uninitialised, the session is unknown, or it
/// was not `Attached` (a second detach is a no-op).
#[must_use]
// The session guard must span `get_mut` + the policy mutation; that is the
// minimum scope, so tightening it further is not possible.
#[allow(clippy::significant_drop_tightening)]
pub fn on_detach(session_id: &str, base_token: ResumeToken) -> Option<ResumeToken> {
    let state = STATE.get()?;
    let now = state.now_ms();
    let unix = now_unix();
    let mut map = state.lock();
    let session = map.get_mut(session_id)?;
    let (decision, token) = LifecyclePolicy::on_detach(session, now, unix, base_token)?;
    tracing::info!(session = %session_id, ?decision, "supervisor: session detached, token annotated");
    Some(token)
}

/// Re-attach a still-live detached session on resume/reattach (WS-supervisor).
///
/// Resets its idle timers so a pending retirement is cancelled. A no-op
/// (returns `false`) when uninitialised, unknown, or not currently detached.
// The session guard must span `get_mut` + the policy mutation.
#[allow(clippy::significant_drop_tightening)]
pub fn on_reattach(session_id: &str) -> bool {
    let Some(state) = STATE.get() else { return false };
    let now = state.now_ms();
    let mut map = state.lock();
    let Some(session) = map.get_mut(session_id) else {
        return false;
    };
    LifecyclePolicy::on_reattach(session, now).is_some_and(|decision| {
        tracing::info!(session = %session_id, ?decision, "supervisor: session reattached");
        true
    })
}

/// One supervisor tick: sample RSS, plan + apply memory shedding, then run an
/// idle-tick per session, cleaning up any retired/shed session's tracking.
///
/// Pure-ish: all destructive actions are gated by [`enforce_enabled`]; by
/// default the only effect is dropping retired/parked entries from the
/// supervisor's own table and logging. A no-op when uninitialised.
fn tick() {
    let Some(state) = STATE.get() else { return };
    let now = state.now_ms();
    let unix = now_unix();

    // Snapshot the table into a Vec for the policy's slice API, holding the lock
    // only for the clone and sorting the owned copy after release.
    let mut sessions: Vec<SessionState> = state.lock().values().cloned().collect();
    if sessions.is_empty() {
        return;
    }
    // Deterministic order so the shed planner's tie-breaking is stable.
    sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));

    // ── Memory-pressure shedding (skipped when RSS is unavailable) ──────────
    if let Some(rss) = sample_rss_bytes() {
        let reading = MemReading::from_rss(rss);
        let plan = state.policy.plan_shed(&sessions, reading, now);
        if !plan.is_empty() {
            let decisions = state
                .policy
                .apply_shed(&mut sessions, &plan, now, unix, |s| base_token_for(&s.session_id));
            for d in &decisions {
                apply_decision(state, d);
            }
        }
    }

    // ── Idle-grace retirement (one tick per session) ───────────────────────
    for s in &mut sessions {
        let decision = state.policy.on_idle_tick(s, now);
        apply_decision(state, &decision);
    }

    // Write the mutated session states back so detach/park/retire transitions
    // recorded above persist for the next tick.
    {
        let mut map = state.lock();
        for s in sessions {
            // Only update entries that still exist (apply_decision may have
            // removed retired/shed ids when enforcement is off).
            if let Some(slot) = map.get_mut(&s.session_id) {
                *slot = s;
            }
        }
    }
}

/// Act on a single policy [`Decision`]. Conservative by default: a `Retire`
/// drops the supervisor's tracking for the session and logs; a `Shed` logs the
/// annotated token (the policy already parked the session in `Detached`). No
/// live connection is killed and no on-disk session is deleted unless an
/// operator opts in via [`ENFORCE_ENV`] — and even then no destructive teardown
/// is wired yet (the daemon owns connection handles per-task, not here), so
/// this stays a logging + bookkeeping boundary that is safe to ship.
fn apply_decision(state: &SupervisorState, decision: &Decision) {
    match decision {
        Decision::Keep { .. } => {}
        Decision::Retire { session_id, reason } => {
            tracing::info!(session = %session_id, ?reason, "supervisor: session retired (idle grace)");
            // Stage C5 Task 4: the supervisor is retiring this session after an
            // inactivity grace lapsed (idle while attached, or detached past its
            // grace) — that is the `Idle` pain bucket. Emitted at the single
            // idle-retire decision site so it fires exactly once per retirement,
            // for both `IdleGrace` and `DetachedGrace` (both are
            // inactivity-timeout closures). Default-off ⇒ no event.
            crate::agent::record_session_stop_pain(crate::agent::SessionStopPain::reason_only(
                origin_telemetry::SessionStopReason::Idle,
            ));
            // Drop our bookkeeping; the real session persists on disk. The
            // foreground interactive session is only retired after the long
            // idle grace, and we never force-close its connection.
            state.lock().remove(session_id);
        }
        Decision::Shed { session_id, token } => log_shed(session_id, token),
        Decision::Handoff { session_id, kind } => {
            tracing::debug!(session = %session_id, ?kind, "supervisor: handoff");
        }
    }
}

/// Log a `Shed` decision (the policy already parked the session in `Detached`).
/// Even under [`ENFORCE_ENV`] no connection-teardown is wired here, because
/// connection handles live in the per-connection task, not in this global
/// table; a future hook can persist `token` and signal that task to
/// checkpoint+suspend. Until then this is a logging boundary.
fn log_shed(session_id: &str, token: &ResumeToken) {
    tracing::info!(
        session = %session_id,
        detached_at_unix = ?token.detached_at_unix,
        memory_estimate_bytes = ?token.memory_estimate_bytes,
        "supervisor: session shed under memory pressure (parked, token annotated)"
    );
    if enforce_enabled() {
        tracing::debug!(session = %session_id, "supervisor: enforce=1 but no teardown hook wired; parked only");
    }
}

/// Build a base [`ResumeToken`] for `session_id` to annotate on shed.
///
/// TODO: thread the daemon's real per-session checkpoint here (the
/// `make_goal_checkpoint_token` path) once the supervisor holds a reference to
/// the live `active_goal`/turn state. Today a session that is shed gets a
/// minimal token (zero CAS root ⇒ "replay from sqlite", the same as a session
/// with no token at all — safe), which is sufficient because the shed path only
/// *parks* (never deletes) and the real checkpoint is already written by the
/// existing turn-boundary checkpoint path.
fn base_token_for(session_id: &str) -> ResumeToken {
    ResumeToken {
        session_id: session_id.to_string(),
        last_turn: 0,
        cas_handle_root: [0u8; 32],
        pending_tool_calls: Vec::new(),
        plan_seq: 0,
        goal: None,
        detached_at_unix: None,
        memory_estimate_bytes: None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)] // test helpers may unwrap/panic on invariants.
mod tests {
    use super::*;

    // These tests exercise the *pure* helpers and the policy folds directly,
    // not the process-global `STATE` (which a tokio tick owns and which would
    // make tests order-dependent). The policy itself is exhaustively unit-
    // tested in `origin-supervisor`; here we prove the daemon adapter's glue.

    #[test]
    fn rss_sampler_is_best_effort() {
        // On Linux it should yield Some(>0); elsewhere None. Either way it must
        // never panic, which is the contract the tick relies on.
        let rss = sample_rss_bytes();
        #[cfg(target_os = "linux")]
        assert!(
            rss.is_none_or(|b| b > 0),
            "linux RSS should be positive when present"
        );
        #[cfg(not(target_os = "linux"))]
        assert!(rss.is_none(), "non-linux RSS is None (mem-shed skipped)");
    }

    #[test]
    fn enforce_defaults_off() {
        // The destructive gate must be opt-in. We don't mutate the env here
        // (parallel-safe); the default in any clean CI shell is unset ⇒ false.
        if std::env::var(ENFORCE_ENV).is_err() {
            assert!(!enforce_enabled(), "enforce must default to off");
        }
    }

    #[test]
    fn detach_then_reattach_round_trips_via_policy() {
        // Mirror the daemon adapter's detach→reattach using the same policy
        // calls the module makes, proving the annotated-token + timer-reset
        // contract the daemon depends on.
        let mut s = SessionState::new("sess-x", SessionClass::Detached, 0);
        let base = base_token_for("sess-x");
        let (decision, token) = LifecyclePolicy::on_detach(&mut s, 1_000, 42, base).unwrap();
        match decision {
            Decision::Handoff { kind, .. } => {
                assert!(matches!(kind, origin_supervisor::HandoffKind::Detached));
            }
            other => panic!("expected detach handoff, got {other:?}"),
        }
        assert_eq!(token.detached_at_unix, Some(42));
        assert!(s.attach.is_detached());

        let re = LifecyclePolicy::on_reattach(&mut s, 5_000).unwrap();
        assert!(matches!(
            re,
            Decision::Handoff {
                kind: origin_supervisor::HandoffKind::Reattached,
                ..
            }
        ));
        assert_eq!(s.last_active_ms, 5_000, "timers reset on reattach");
    }

    #[test]
    fn base_token_is_replay_from_sqlite_shape() {
        let t = base_token_for("s");
        assert_eq!(t.session_id, "s");
        assert_eq!(t.cas_handle_root, [0u8; 32]);
        assert!(t.goal.is_none());
        assert!(t.pending_tool_calls.is_empty());
    }
}
