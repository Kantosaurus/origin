// SPDX-License-Identifier: Apache-2.0
//! Supervisor lifecycle **policy** layer — a pure, deterministic state machine
//! that decides when to *shed*, *retire*, or *hand off* agent sessions.
//!
//! # Why a pure policy
//!
//! The supervisor's crash-restart loop already keeps a single daemon alive.
//! What was missing — and what this module supplies — is the decision logic
//! for three lifecycle pressures that arrive once many sessions coexist under
//! one daemon:
//!
//! 1. **Memory-pressure shedding.** When the daemon's resident set approaches a
//!    configured budget, *some* sessions must be paused/retired to relieve it.
//!    The foreground interactive session is sacrosanct; background/detached
//!    sessions are shed first, oldest-idle first.
//! 2. **Idle-grace retirement.** A session idle past a grace window is retired.
//!    Pinned/detached sessions get a longer grace so a user who walked away can
//!    re-attach.
//! 3. **Attach/detach handoff.** A session whose client disconnected is kept
//!    alive in a `Detached` state, its transcript preserved via the existing
//!    [`crate::resume_token::ResumeToken`] mechanism, and can be re-attached by
//!    a new client (resetting its timers) until its detached grace expires.
//!
//! # Determinism by construction
//!
//! The policy performs **no I/O**: no real RSS reads, no `Instant::now()`, no
//! process killing. Every time-dependent input is passed in as a *monotonic
//! millisecond clock value* (`now_ms`), and every memory input is passed in as
//! a [`MemReading`]. The daemon (Phase 2) supplies the real clock and a real
//! RSS sampler; the policy just folds events into state and emits
//! [`Decision`]s. This keeps the whole module unit-testable with hand-built
//! timelines and zero platform concerns.
//!
//! # Env-overridable knobs
//!
//! [`LifecycleConfig::from_env`] reads the following, falling back to
//! [`LifecycleConfig::DEFAULT`] for any unset/unparseable value:
//!
//! - `ORIGIN_SUPERVISOR_IDLE_GRACE_MS` — interactive idle grace (ms).
//! - `ORIGIN_SUPERVISOR_DETACHED_GRACE_MS` — detached/pinned idle grace (ms).
//! - `ORIGIN_SUPERVISOR_MEM_BUDGET_MB` — soft memory budget (MiB).
//! - `ORIGIN_SUPERVISOR_MEM_SHED_RATIO` — pressure ratio (0..1) at which
//!   shedding begins, relative to the budget.
//!
//! The [`LifecyclePolicy`] struct itself only ever takes a fully-built
//! [`LifecycleConfig`]; env parsing is isolated so the policy stays pure.

// `LifecycleConfig`, `LifecyclePolicy`, `SessionState`, etc. intentionally
// carry the module's name: they are the crate's public lifecycle vocabulary the
// daemon consumes via `origin_supervisor::Lifecycle*`, so the repetition is by
// design rather than accidental noise.
#![allow(clippy::module_name_repetitions)]

use crate::resume_token::ResumeToken;

/// One mebibyte in bytes.
const MIB: u64 = 1024 * 1024;

/// How a session is being used, which governs how aggressively it may be shed
/// or retired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionClass {
    /// The session a human is actively driving. Never shed for memory; retired
    /// only after the (short) interactive idle grace.
    Interactive,
    /// A background / detached / pinned session — kept alive without an
    /// attached client. Shed first under memory pressure; retired only after
    /// the (longer) detached grace.
    Detached,
}

impl SessionClass {
    /// `true` for the always-protected foreground class.
    #[must_use]
    pub const fn is_interactive(self) -> bool {
        matches!(self, Self::Interactive)
    }
}

/// A single point-in-time memory reading for the daemon as a whole.
///
/// Either field alone is enough to drive shedding; supplying both lets the
/// policy use whichever signal is more conservative (higher pressure):
/// `pressure()` takes the max of the explicit `ratio` and `rss_bytes /
/// budget`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemReading {
    /// Resident set size of the daemon (and its sessions) in bytes.
    pub rss_bytes: u64,
    /// Optional pre-computed pressure ratio in `0..=1+` (e.g. from a cgroup
    /// `memory.current / memory.max`). `None` means "derive it from RSS".
    pub ratio: Option<f64>,
}

impl MemReading {
    /// Construct a reading from an RSS byte count, with no external ratio.
    #[must_use]
    pub const fn from_rss(rss_bytes: u64) -> Self {
        Self {
            rss_bytes,
            ratio: None,
        }
    }

    /// Construct a reading from an externally-supplied pressure ratio.
    #[must_use]
    pub const fn from_ratio(ratio: f64) -> Self {
        Self {
            rss_bytes: 0,
            ratio: Some(ratio),
        }
    }

    /// Effective pressure ratio against `budget_bytes`, in `0..` (can exceed
    /// `1.0` when over budget). Takes the larger of the explicit ratio and the
    /// RSS-derived ratio so neither signal is masked. A zero/!-finite budget
    /// yields the explicit ratio (or `0.0`), never a division by zero.
    #[must_use]
    pub fn pressure(self, budget_bytes: u64) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let rss_ratio = if budget_bytes == 0 {
            0.0
        } else {
            self.rss_bytes as f64 / budget_bytes as f64
        };
        let explicit = self.ratio.filter(|r| r.is_finite()).unwrap_or(0.0);
        rss_ratio.max(explicit)
    }
}

/// Tunable thresholds for the lifecycle policy. All durations are in
/// milliseconds of the injected monotonic clock; the memory budget is in bytes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LifecycleConfig {
    /// Idle window after which an [`SessionClass::Interactive`] session is
    /// retired.
    pub idle_grace_ms: u64,
    /// Idle window after which a [`SessionClass::Detached`] session is retired.
    /// Always `>=` `idle_grace_ms` in practice; the longer grace lets a user
    /// re-attach.
    pub detached_grace_ms: u64,
    /// Soft memory budget in bytes. Pressure is measured relative to this.
    pub mem_budget_bytes: u64,
    /// Pressure ratio (relative to the budget) at or above which shedding
    /// begins. e.g. `0.9` sheds once usage crosses 90% of budget.
    pub mem_shed_ratio: f64,
}

impl LifecycleConfig {
    /// Built-in defaults: 5-minute interactive grace, 30-minute detached grace,
    /// 1 GiB budget, shed at 90%.
    pub const DEFAULT: Self = Self {
        idle_grace_ms: 5 * 60 * 1_000,
        detached_grace_ms: 30 * 60 * 1_000,
        mem_budget_bytes: 1024 * MIB,
        mem_shed_ratio: 0.9,
    };

    /// Build a config from process environment, falling back to
    /// [`Self::DEFAULT`] for any unset or unparseable variable.
    ///
    /// Reads `ORIGIN_SUPERVISOR_IDLE_GRACE_MS`,
    /// `ORIGIN_SUPERVISOR_DETACHED_GRACE_MS`,
    /// `ORIGIN_SUPERVISOR_MEM_BUDGET_MB`, and
    /// `ORIGIN_SUPERVISOR_MEM_SHED_RATIO`.
    #[must_use]
    pub fn from_env() -> Self {
        let d = Self::DEFAULT;
        let idle_grace_ms = env_u64("ORIGIN_SUPERVISOR_IDLE_GRACE_MS", d.idle_grace_ms);
        let detached_grace_ms = env_u64("ORIGIN_SUPERVISOR_DETACHED_GRACE_MS", d.detached_grace_ms);
        let mem_budget_bytes =
            env_u64("ORIGIN_SUPERVISOR_MEM_BUDGET_MB", d.mem_budget_bytes / MIB).saturating_mul(MIB);
        let mem_shed_ratio = env_f64("ORIGIN_SUPERVISOR_MEM_SHED_RATIO", d.mem_shed_ratio);
        Self {
            idle_grace_ms,
            detached_grace_ms,
            mem_budget_bytes,
            mem_shed_ratio,
        }
        .sanitized()
    }

    /// Clamp the config into a self-consistent shape: a non-zero budget, a shed
    /// ratio in `(0, 4]`, and `detached_grace_ms >= idle_grace_ms` (a detached
    /// session must never be retired *sooner* than an interactive one).
    #[must_use]
    pub fn sanitized(self) -> Self {
        let mem_budget_bytes = self.mem_budget_bytes.max(1);
        let mem_shed_ratio = if self.mem_shed_ratio.is_finite() && self.mem_shed_ratio > 0.0 {
            self.mem_shed_ratio.min(4.0)
        } else {
            Self::DEFAULT.mem_shed_ratio
        };
        let detached_grace_ms = self.detached_grace_ms.max(self.idle_grace_ms);
        Self {
            idle_grace_ms: self.idle_grace_ms,
            detached_grace_ms,
            mem_budget_bytes,
            mem_shed_ratio,
        }
    }

    /// The grace window (ms) that applies to a session of `class`.
    #[must_use]
    pub const fn grace_for(&self, class: SessionClass) -> u64 {
        match class {
            SessionClass::Interactive => self.idle_grace_ms,
            SessionClass::Detached => self.detached_grace_ms,
        }
    }
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

fn env_u64(key: &str, fallback: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(fallback)
}

fn env_f64(key: &str, fallback: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(fallback)
}

/// Where a session sits in the attach/detach state machine.
///
/// ```text
///        detach          idle past detached_grace
/// Attached ----> Detached ----------------------> Retired
///    ^              |
///    +--------------+
///        reattach (resets timers)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachState {
    /// A client is connected and driving the session.
    Attached,
    /// The client disconnected; the session is kept warm. `since_ms` is the
    /// monotonic clock value at which it entered this state, used to measure
    /// the detached grace.
    Detached {
        /// Monotonic clock value (ms) at which the session detached.
        since_ms: u64,
    },
    /// Terminal: the session has been retired and its slot reclaimed.
    Retired,
}

impl AttachState {
    /// `true` once the session has been retired.
    #[must_use]
    pub const fn is_retired(self) -> bool {
        matches!(self, Self::Retired)
    }

    /// `true` while the session is detached (client gone, kept warm).
    #[must_use]
    pub const fn is_detached(self) -> bool {
        matches!(self, Self::Detached { .. })
    }
}

/// The supervisor's view of one live session. Pure data — the policy mutates
/// it deterministically from events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionState {
    /// Stable session identifier (matches `ResumeToken::session_id`).
    pub session_id: String,
    /// Usage class governing shed/retire aggressiveness.
    pub class: SessionClass,
    /// Attach/detach machine position.
    pub attach: AttachState,
    /// Monotonic clock value (ms) of the session's last activity. Reset on any
    /// activity tick and on re-attach.
    pub last_active_ms: u64,
    /// Most recent resident-memory estimate for this session, in bytes. Used
    /// only to annotate handoff tokens; shedding order is by idle age, not by
    /// per-session RSS (which the policy does not require).
    pub rss_estimate_bytes: u64,
}

impl SessionState {
    /// A freshly-attached session, last active *now*.
    #[must_use]
    pub fn new(session_id: impl Into<String>, class: SessionClass, now_ms: u64) -> Self {
        Self {
            session_id: session_id.into(),
            class,
            attach: AttachState::Attached,
            last_active_ms: now_ms,
            rss_estimate_bytes: 0,
        }
    }

    /// Milliseconds the session has been idle as of `now_ms` (saturating).
    #[must_use]
    pub const fn idle_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.last_active_ms)
    }

    /// `true` if this session can be shed to relieve memory pressure: never the
    /// foreground interactive session, never an already-retired one.
    #[must_use]
    pub const fn is_sheddable(&self) -> bool {
        !self.attach.is_retired() && !self.class.is_interactive()
    }
}

/// A lifecycle decision the supervisor should act on. The policy emits these;
/// the daemon applies them (pausing/killing processes, persisting tokens).
///
/// Note: not `PartialEq` because the [`Self::Shed`] variant embeds a
/// [`ResumeToken`], whose foreign type does not implement `PartialEq`. Match on
/// the variant and inspect fields instead of comparing whole `Decision`s.
#[derive(Debug, Clone)]
pub enum Decision {
    /// Keep the session as-is. Carries the reason for observability.
    Keep {
        /// Session the decision applies to.
        session_id: String,
    },
    /// Shed (pause/retire) the session to relieve memory pressure. The supplied
    /// [`ResumeToken`] should be persisted so the session can be re-hydrated
    /// later; it carries `detached_at_unix`/`memory_estimate_bytes` annotations.
    /// Boxed so this variant does not bloat the whole enum.
    Shed {
        /// Session being shed.
        session_id: String,
        /// Handoff token to persist before pausing the session.
        token: Box<ResumeToken>,
    },
    /// Retire the session: it exceeded its idle grace (or its detached grace).
    Retire {
        /// Session being retired.
        session_id: String,
        /// Why it was retired.
        reason: RetireReason,
    },
    /// Hand the session off across an attach boundary (detach or re-attach).
    Handoff {
        /// Session involved in the handoff.
        session_id: String,
        /// Which direction the handoff went.
        kind: HandoffKind,
    },
}

impl Decision {
    /// The session id this decision concerns.
    #[must_use]
    pub fn session_id(&self) -> &str {
        match self {
            Self::Keep { session_id }
            | Self::Shed { session_id, .. }
            | Self::Retire { session_id, .. }
            | Self::Handoff { session_id, .. } => session_id,
        }
    }
}

/// Why a session was retired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetireReason {
    /// Idle past the interactive grace while attached.
    IdleGrace,
    /// Idle past the detached grace while detached (client never came back).
    DetachedGrace,
}

/// The direction of an attach-boundary handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffKind {
    /// Client disconnected; session moved `Attached -> Detached`.
    Detached,
    /// New client connected; session moved `Detached -> Attached`, timers reset.
    Reattached,
}

/// The lifecycle policy.
///
/// A fold over events into [`SessionState`] plus the shedding planner. Holds
/// only the (immutable) config; all session state lives in the caller-owned
/// [`SessionState`] slice so the policy stays a pure function family.
#[derive(Debug, Clone)]
pub struct LifecyclePolicy {
    config: LifecycleConfig,
}

impl LifecyclePolicy {
    /// Construct a policy from a (typically env-derived) config, sanitizing it.
    #[must_use]
    pub fn new(config: LifecycleConfig) -> Self {
        Self {
            config: config.sanitized(),
        }
    }

    /// The effective config in use.
    #[must_use]
    pub const fn config(&self) -> &LifecycleConfig {
        &self.config
    }

    /// Record activity on a session: refresh its `last_active_ms`. A no-op for
    /// retired sessions.
    pub const fn on_activity(session: &mut SessionState, now_ms: u64) {
        if !session.attach.is_retired() {
            session.last_active_ms = now_ms;
        }
    }

    /// Move an attached session to `Detached`, preserving state for later
    /// re-attach. Returns the handoff decision plus a [`ResumeToken`] annotated
    /// with the detach time/memory so the daemon can persist it.
    ///
    /// A no-op (returns `None`) if the session is not currently `Attached`.
    #[must_use]
    pub fn on_detach(
        session: &mut SessionState,
        now_ms: u64,
        now_unix: u64,
        mut token: ResumeToken,
    ) -> Option<(Decision, ResumeToken)> {
        if session.attach != AttachState::Attached {
            return None;
        }
        session.attach = AttachState::Detached { since_ms: now_ms };
        // The detach clock also acts as the idle anchor for the detached grace.
        session.last_active_ms = now_ms;
        token.detached_at_unix = Some(now_unix);
        token.memory_estimate_bytes = Some(session.rss_estimate_bytes);
        let decision = Decision::Handoff {
            session_id: session.session_id.clone(),
            kind: HandoffKind::Detached,
        };
        Some((decision, token))
    }

    /// Re-attach a detached session: move `Detached -> Attached` and reset its
    /// idle timers so retirement is cancelled. Returns the handoff decision.
    ///
    /// A no-op (returns `None`) if the session is not currently `Detached`.
    #[must_use]
    pub fn on_reattach(session: &mut SessionState, now_ms: u64) -> Option<Decision> {
        if !session.attach.is_detached() {
            return None;
        }
        session.attach = AttachState::Attached;
        session.last_active_ms = now_ms;
        Some(Decision::Handoff {
            session_id: session.session_id.clone(),
            kind: HandoffKind::Reattached,
        })
    }

    /// Evaluate a single idle tick against one session, mutating it to
    /// `Retired` if it crossed its grace window.
    ///
    /// - Attached sessions retire after [`LifecycleConfig::idle_grace_ms`].
    /// - Detached sessions retire after [`LifecycleConfig::detached_grace_ms`]
    ///   (measured from the detach instant, since `on_detach` anchors
    ///   `last_active_ms` there).
    /// - Already-retired sessions are kept (idempotent).
    #[must_use]
    pub fn on_idle_tick(&self, session: &mut SessionState, now_ms: u64) -> Decision {
        if session.attach.is_retired() {
            return Decision::Keep {
                session_id: session.session_id.clone(),
            };
        }
        let idle = session.idle_ms(now_ms);
        let (grace, reason) = match session.attach {
            AttachState::Detached { .. } => (self.config.detached_grace_ms, RetireReason::DetachedGrace),
            // Attached (Retired handled above).
            _ => (self.config.idle_grace_ms, RetireReason::IdleGrace),
        };
        if idle >= grace {
            session.attach = AttachState::Retired;
            Decision::Retire {
                session_id: session.session_id.clone(),
                reason,
            }
        } else {
            Decision::Keep {
                session_id: session.session_id.clone(),
            }
        }
    }

    /// Plan a memory-pressure shed pass over all live sessions.
    ///
    /// Returns the ordered list of session indices (into `sessions`) that
    /// should be shed to bring projected usage below the shed threshold. The
    /// order is the eviction order:
    ///
    /// 1. Only [`SessionState::is_sheddable`] sessions are eligible — the
    ///    foreground interactive session and retired sessions are excluded.
    /// 2. Eligible sessions are shed **oldest-idle first** (largest
    ///    `idle_ms(now_ms)`), ties broken by `session_id` for determinism.
    /// 3. Shedding stops as soon as the *estimated* relief (sum of shed
    ///    sessions' `rss_estimate_bytes`) brings usage at/below the shed
    ///    threshold, or when no eligible sessions remain.
    ///
    /// If the reading is below the shed threshold, returns an empty plan.
    /// This method does **not** mutate the sessions; the caller applies the
    /// plan via [`Self::apply_shed`] once it has the handoff tokens.
    #[must_use]
    pub fn plan_shed(&self, sessions: &[SessionState], reading: MemReading, now_ms: u64) -> Vec<usize> {
        let pressure = reading.pressure(self.config.mem_budget_bytes);
        if pressure < self.config.mem_shed_ratio {
            return Vec::new();
        }
        // Target byte usage that would put us back at the shed threshold.
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let target_bytes = (self.config.mem_shed_ratio * self.config.mem_budget_bytes as f64) as u64;
        let mut current = reading.rss_bytes;

        // Eligible indices, sorted oldest-idle first, then by id for stability.
        let mut eligible: Vec<usize> = (0..sessions.len())
            .filter(|&i| sessions[i].is_sheddable())
            .collect();
        eligible.sort_by(|&a, &b| {
            let ia = sessions[a].idle_ms(now_ms);
            let ib = sessions[b].idle_ms(now_ms);
            ib.cmp(&ia)
                .then_with(|| sessions[a].session_id.cmp(&sessions[b].session_id))
        });

        let mut plan = Vec::new();
        for idx in eligible {
            if current <= target_bytes {
                break;
            }
            current = current.saturating_sub(sessions[idx].rss_estimate_bytes);
            plan.push(idx);
        }
        plan
    }

    /// Apply a shed plan: move each planned session to `Detached` (if attached)
    /// and emit a [`Decision::Shed`] carrying an annotated handoff token.
    ///
    /// `token_for` supplies the base [`ResumeToken`] for a given session id (the
    /// daemon's checkpoint); the policy annotates it with detach time/memory.
    /// Planned indices that are out of range or already retired are skipped.
    #[must_use]
    pub fn apply_shed<F>(
        &self,
        sessions: &mut [SessionState],
        plan: &[usize],
        now_ms: u64,
        now_unix: u64,
        mut token_for: F,
    ) -> Vec<Decision>
    where
        F: FnMut(&SessionState) -> ResumeToken,
    {
        let mut out = Vec::with_capacity(plan.len());
        for &idx in plan {
            let Some(session) = sessions.get_mut(idx) else {
                continue;
            };
            if session.attach.is_retired() {
                continue;
            }
            let mut token = token_for(session);
            token.detached_at_unix = Some(now_unix);
            token.memory_estimate_bytes = Some(session.rss_estimate_bytes);
            // Shedding parks the session in the Detached state so it remains
            // re-attachable until its detached grace lapses.
            if session.attach == AttachState::Attached {
                session.attach = AttachState::Detached { since_ms: now_ms };
            }
            session.last_active_ms = now_ms;
            out.push(Decision::Shed {
                session_id: session.session_id.clone(),
                token: Box::new(token),
            });
        }
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp, clippy::panic)]
mod tests {
    use super::*;

    fn token_for(id: &str) -> ResumeToken {
        ResumeToken {
            session_id: id.to_string(),
            last_turn: 0,
            cas_handle_root: [0u8; 32],
            pending_tool_calls: Vec::new(),
            plan_seq: 0,
            goal: None,
            detached_at_unix: None,
            memory_estimate_bytes: None,
        }
    }

    fn sess(id: &str, class: SessionClass, last_active_ms: u64, rss: u64) -> SessionState {
        SessionState {
            session_id: id.to_string(),
            class,
            attach: AttachState::Attached,
            last_active_ms,
            rss_estimate_bytes: rss,
        }
    }

    // ---- config / env ------------------------------------------------------

    #[test]
    fn default_config_is_sane() {
        let c = LifecycleConfig::DEFAULT;
        assert!(c.detached_grace_ms >= c.idle_grace_ms);
        assert!(c.mem_budget_bytes > 0);
        assert!(c.mem_shed_ratio > 0.0 && c.mem_shed_ratio <= 1.0);
        // sanitized() is idempotent on the default.
        assert_eq!(c.sanitized(), c);
    }

    #[test]
    fn sanitize_clamps_detached_grace_up_to_idle() {
        let c = LifecycleConfig {
            idle_grace_ms: 10_000,
            detached_grace_ms: 1_000, // shorter than idle: nonsensical
            mem_budget_bytes: 0,      // must clamp to >= 1
            mem_shed_ratio: -1.0,     // invalid: must fall back
        }
        .sanitized();
        assert_eq!(c.detached_grace_ms, 10_000);
        assert!(c.mem_budget_bytes >= 1);
        assert_eq!(c.mem_shed_ratio, LifecycleConfig::DEFAULT.mem_shed_ratio);
    }

    // ---- memory pressure math ---------------------------------------------

    #[test]
    fn pressure_takes_max_of_signals() {
        let budget = 100 * MIB;
        // RSS at 50%, explicit ratio at 0.95 -> use the higher (0.95).
        let r = MemReading {
            rss_bytes: 50 * MIB,
            ratio: Some(0.95),
        };
        assert!((r.pressure(budget) - 0.95).abs() < 1e-9);
        // RSS at 80%, no explicit -> 0.8.
        let r2 = MemReading::from_rss(80 * MIB);
        assert!((r2.pressure(budget) - 0.8).abs() < 1e-9);
    }

    #[test]
    fn pressure_handles_zero_budget() {
        let r = MemReading::from_rss(123);
        assert_eq!(r.pressure(0), 0.0);
        let r2 = MemReading::from_ratio(0.7);
        assert!((r2.pressure(0) - 0.7).abs() < 1e-9);
    }

    // ---- shedding ----------------------------------------------------------

    #[test]
    fn shed_picks_background_before_interactive() {
        let policy = LifecyclePolicy::new(LifecycleConfig {
            mem_budget_bytes: 100 * MIB,
            mem_shed_ratio: 0.9,
            ..LifecycleConfig::DEFAULT
        });
        // Interactive is the most idle, but must NEVER be shed.
        let sessions = vec![
            sess("fg", SessionClass::Interactive, 0, 40 * MIB), // idle 1000
            sess("bg", SessionClass::Detached, 500, 40 * MIB),  // idle 500
        ];
        // 95% pressure, need to drop below 90 MiB.
        let reading = MemReading::from_rss(95 * MIB);
        let plan = policy.plan_shed(&sessions, reading, 1_000);
        let ids: Vec<&str> = plan.iter().map(|&i| sessions[i].session_id.as_str()).collect();
        assert_eq!(ids, vec!["bg"], "must shed background, never foreground");
    }

    #[test]
    fn shed_is_oldest_idle_first() {
        let policy = LifecyclePolicy::new(LifecycleConfig {
            mem_budget_bytes: 100 * MIB,
            mem_shed_ratio: 0.9,
            ..LifecycleConfig::DEFAULT
        });
        // Three background sessions; each frees only 10 MiB so we need two.
        // older has bigger idle (smaller last_active).
        let sessions = vec![
            sess("newest", SessionClass::Detached, 900, 10 * MIB), // idle 100
            sess("oldest", SessionClass::Detached, 100, 10 * MIB), // idle 900
            sess("middle", SessionClass::Detached, 500, 10 * MIB), // idle 500
        ];
        // 95 MiB used, target 90 MiB -> need to free >5 MiB -> shed 1 session,
        // but each frees 10, so just the single oldest.
        let reading = MemReading::from_rss(95 * MIB);
        let plan = policy.plan_shed(&sessions, reading, 1_000);
        let ids: Vec<&str> = plan.iter().map(|&i| sessions[i].session_id.as_str()).collect();
        assert_eq!(ids, vec!["oldest"]);
    }

    #[test]
    fn shed_continues_until_under_threshold() {
        let policy = LifecyclePolicy::new(LifecycleConfig {
            mem_budget_bytes: 100 * MIB,
            mem_shed_ratio: 0.9,
            ..LifecycleConfig::DEFAULT
        });
        // Each bg frees 10 MiB. Used 130 MiB, target 90 -> free >=40 -> 4 sheds,
        // oldest first.
        let sessions = vec![
            sess("a", SessionClass::Detached, 100, 10 * MIB), // idle 900 (oldest)
            sess("b", SessionClass::Detached, 200, 10 * MIB), // idle 800
            sess("c", SessionClass::Detached, 300, 10 * MIB), // idle 700
            sess("d", SessionClass::Detached, 400, 10 * MIB), // idle 600
            sess("e", SessionClass::Detached, 500, 10 * MIB), // idle 500
        ];
        let reading = MemReading::from_rss(130 * MIB);
        let plan = policy.plan_shed(&sessions, reading, 1_000);
        let ids: Vec<&str> = plan.iter().map(|&i| sessions[i].session_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn no_shed_below_threshold() {
        let policy = LifecyclePolicy::new(LifecycleConfig {
            mem_budget_bytes: 100 * MIB,
            mem_shed_ratio: 0.9,
            ..LifecycleConfig::DEFAULT
        });
        let sessions = vec![sess("bg", SessionClass::Detached, 0, 50 * MIB)];
        let reading = MemReading::from_rss(80 * MIB); // 80% < 90%
        assert!(policy.plan_shed(&sessions, reading, 1_000).is_empty());
    }

    #[test]
    fn apply_shed_parks_detached_and_annotates_token() {
        let policy = LifecyclePolicy::new(LifecycleConfig {
            mem_budget_bytes: 100 * MIB,
            mem_shed_ratio: 0.9,
            ..LifecycleConfig::DEFAULT
        });
        let mut sessions = vec![sess("bg", SessionClass::Detached, 0, 40 * MIB)];
        let reading = MemReading::from_rss(95 * MIB);
        let plan = policy.plan_shed(&sessions, reading, 1_000);
        let decisions = policy.apply_shed(&mut sessions, &plan, 1_000, 1_717_000_000, |s| {
            token_for(&s.session_id)
        });
        assert_eq!(decisions.len(), 1);
        match &decisions[0] {
            Decision::Shed { session_id, token } => {
                assert_eq!(session_id, "bg");
                assert_eq!(token.detached_at_unix, Some(1_717_000_000));
                assert_eq!(token.memory_estimate_bytes, Some(40 * MIB));
            }
            other => panic!("expected Shed, got {other:?}"),
        }
        // Parked, not retired -> still re-attachable.
        assert!(sessions[0].attach.is_detached());
    }

    // ---- idle-grace retirement --------------------------------------------

    #[test]
    fn idle_grace_keeps_within_window() {
        let policy = LifecyclePolicy::new(LifecycleConfig {
            idle_grace_ms: 1_000,
            ..LifecycleConfig::DEFAULT
        });
        let mut s = sess("i", SessionClass::Interactive, 0, 0);
        let d = policy.on_idle_tick(&mut s, 999); // just under grace
        assert!(matches!(d, Decision::Keep { .. }));
        assert_eq!(s.attach, AttachState::Attached);
    }

    #[test]
    fn idle_grace_retires_past_window() {
        let policy = LifecyclePolicy::new(LifecycleConfig {
            idle_grace_ms: 1_000,
            ..LifecycleConfig::DEFAULT
        });
        let mut s = sess("i", SessionClass::Interactive, 0, 0);
        let d = policy.on_idle_tick(&mut s, 1_000); // at grace boundary
        match d {
            Decision::Retire { session_id, reason } => {
                assert_eq!(session_id, "i");
                assert_eq!(reason, RetireReason::IdleGrace);
            }
            other => panic!("expected Retire, got {other:?}"),
        }
        assert!(s.attach.is_retired());
    }

    #[test]
    fn retire_is_idempotent() {
        let policy = LifecyclePolicy::new(LifecycleConfig {
            idle_grace_ms: 1_000,
            ..LifecycleConfig::DEFAULT
        });
        let mut s = sess("i", SessionClass::Interactive, 0, 0);
        let _ = policy.on_idle_tick(&mut s, 5_000);
        assert!(s.attach.is_retired());
        let d = policy.on_idle_tick(&mut s, 10_000);
        assert!(matches!(d, Decision::Keep { .. }), "retired stays retired");
    }

    #[test]
    fn detached_uses_longer_grace() {
        let policy = LifecyclePolicy::new(LifecycleConfig {
            idle_grace_ms: 1_000,
            detached_grace_ms: 10_000,
            ..LifecycleConfig::DEFAULT
        });
        let mut s = sess("d", SessionClass::Detached, 0, 0);
        // Detach at t=0.
        let _ = LifecyclePolicy::on_detach(&mut s, 0, 1_000, token_for("d")).unwrap();
        // At t=5000: past interactive grace (1000) but under detached (10000).
        let d = policy.on_idle_tick(&mut s, 5_000);
        assert!(matches!(d, Decision::Keep { .. }), "detached survives longer");
        // At t=10000: hits detached grace.
        let d = policy.on_idle_tick(&mut s, 10_000);
        assert!(matches!(
            d,
            Decision::Retire {
                reason: RetireReason::DetachedGrace,
                ..
            }
        ));
    }

    // ---- attach / detach handoff ------------------------------------------

    #[test]
    fn detach_grace_retire_timeline() {
        let policy = LifecyclePolicy::new(LifecycleConfig {
            idle_grace_ms: 1_000,
            detached_grace_ms: 5_000,
            ..LifecycleConfig::DEFAULT
        });
        let mut s = sess("h", SessionClass::Detached, 0, 7 * MIB);

        // 1) Detach at t=2000 -> Handoff(Detached), token annotated.
        let (decision, token) = LifecyclePolicy::on_detach(&mut s, 2_000, 999, token_for("h")).unwrap();
        assert!(matches!(
            decision,
            Decision::Handoff {
                kind: HandoffKind::Detached,
                ..
            }
        ));
        assert_eq!(token.detached_at_unix, Some(999));
        assert_eq!(token.memory_estimate_bytes, Some(7 * MIB));
        assert!(s.attach.is_detached());

        // 2) Within grace (t=6000, 4000 since detach) -> Keep.
        let d = policy.on_idle_tick(&mut s, 6_000);
        assert!(matches!(d, Decision::Keep { .. }));

        // 3) Past grace (t=7000, 5000 since detach) -> Retire(DetachedGrace).
        let d = policy.on_idle_tick(&mut s, 7_000);
        assert!(matches!(
            d,
            Decision::Retire {
                reason: RetireReason::DetachedGrace,
                ..
            }
        ));
        assert!(s.attach.is_retired());
    }

    #[test]
    fn reattach_cancels_retirement() {
        let policy = LifecyclePolicy::new(LifecycleConfig {
            idle_grace_ms: 1_000,
            detached_grace_ms: 5_000,
            ..LifecycleConfig::DEFAULT
        });
        let mut s = sess("h", SessionClass::Detached, 0, 0);
        let _ = LifecyclePolicy::on_detach(&mut s, 1_000, 0, token_for("h")).unwrap();

        // Re-attach at t=4000 (still within the 5000 grace).
        let d = LifecyclePolicy::on_reattach(&mut s, 4_000).unwrap();
        assert!(matches!(
            d,
            Decision::Handoff {
                kind: HandoffKind::Reattached,
                ..
            }
        ));
        assert_eq!(s.attach, AttachState::Attached);
        assert_eq!(s.last_active_ms, 4_000, "timers reset on reattach");

        // Now an idle tick at t=4500 must NOT retire (only 500ms since reattach,
        // under the 1000ms interactive grace).
        let d = policy.on_idle_tick(&mut s, 4_500);
        assert!(
            matches!(d, Decision::Keep { .. }),
            "reattach cancelled retirement"
        );
    }

    #[test]
    fn detach_when_not_attached_is_noop() {
        let mut s = sess("x", SessionClass::Detached, 0, 0);
        let _ = LifecyclePolicy::on_detach(&mut s, 0, 0, token_for("x")).unwrap();
        // Already detached -> a second detach is a no-op.
        assert!(LifecyclePolicy::on_detach(&mut s, 1, 1, token_for("x")).is_none());
    }

    #[test]
    fn reattach_when_attached_is_noop() {
        let mut s = sess("x", SessionClass::Interactive, 0, 0);
        assert!(LifecyclePolicy::on_reattach(&mut s, 10).is_none());
    }

    #[test]
    fn on_activity_resets_idle_but_not_retired() {
        let mut s = sess("a", SessionClass::Interactive, 0, 0);
        LifecyclePolicy::on_activity(&mut s, 500);
        assert_eq!(s.last_active_ms, 500);
        s.attach = AttachState::Retired;
        LifecyclePolicy::on_activity(&mut s, 9_999);
        assert_eq!(s.last_active_ms, 500, "retired session activity ignored");
    }

    #[test]
    fn decision_session_id_accessor() {
        let d = Decision::Keep {
            session_id: "z".into(),
        };
        assert_eq!(d.session_id(), "z");
    }
}
