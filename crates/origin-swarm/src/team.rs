// SPDX-License-Identifier: Apache-2.0
//! Named agent-team primitives (WS-C; claude-code L129, cline L167).
//!
//! This module is the *vocabulary* layer that sits on top of the real swarm
//! worker substrate ([`crate::Coordinator`] / [`crate::worker`]). A
//! [`Coordinator`](crate::Coordinator) already spawns workers and aggregates
//! their [`CompletionReport`](crate::CompletionReport)s; this adds the
//! named-teammate bookkeeping a future daemon/cli layer needs to *talk about*
//! those workers as a persistent team:
//!
//! - [`Team`] — a named team `{ name, coordinator, teammates }` with register /
//!   lookup-by-name / list-idle helpers.
//! - [`Teammate`] / [`TeammateStatus`] — a named worker and its current state
//!   (`Idle` / `Working { task }` / `Done`).
//! - [`TeamEvent`] — the two lifecycle events claude-code surfaces:
//!   [`TeamEvent::TeammateIdle`] and [`TeamEvent::TaskCompleted`].
//! - [`MissionLog`] / [`MissionEntry`] — an append-only timeline the team
//!   writes as work progresses, with a plain-text [`MissionLog::render`].
//! - [`TeamRegistry`] — creates/looks-up teams by name and drives status
//!   transitions (assign ⇒ `Working`, complete ⇒ `Done` + a `TaskCompleted`
//!   event, mark-idle ⇒ `Idle` + a `TeammateIdle` event), journaling each to the
//!   log. It also owns one [`Mailbox`](crate::collab::Mailbox) per teammate so
//!   teammates can DM each other, reusing the WS-L collab primitives.
//!
//! Everything here is pure and crate-local: no IO, no daemon coupling, no
//! async. The intent is that a future daemon/cli layer drives a [`Team`] via
//! the existing real-worker [`Coordinator`](crate::Coordinator); this module is
//! purely the shared vocabulary plus the in-memory bookkeeping.

use std::collections::HashMap;
use std::fmt::Write as _;

use thiserror::Error;

use crate::collab::{Mailbox, Message};
use crate::coordinator::WorkerId;

/// Current state of a [`Teammate`] within a [`Team`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TeammateStatus {
    /// Available for assignment; surfaced by [`Team::idle_teammates`].
    #[default]
    Idle,
    /// Actively working on `task`.
    Working {
        /// The task description the teammate is working on.
        task: String,
    },
    /// Finished its assigned work and reported back.
    Done,
}

impl TeammateStatus {
    /// `true` iff the teammate is [`TeammateStatus::Idle`].
    #[must_use]
    pub const fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    /// `true` iff the teammate is [`TeammateStatus::Working`].
    #[must_use]
    pub const fn is_working(&self) -> bool {
        matches!(self, Self::Working { .. })
    }

    /// `true` iff the teammate is [`TeammateStatus::Done`].
    #[must_use]
    pub const fn is_done(&self) -> bool {
        matches!(self, Self::Done)
    }
}

/// A named worker that belongs to a [`Team`].
///
/// The `id` is the real [`WorkerId`] the [`Coordinator`](crate::Coordinator)
/// hands out on spawn; `name` is the human-facing handle the team and the
/// mission log refer to it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Teammate {
    /// The underlying real-worker id.
    pub id: WorkerId,
    /// Human-facing name (unique within a [`Team`]).
    pub name: String,
    /// Current lifecycle status.
    pub status: TeammateStatus,
}

impl Teammate {
    /// Construct an idle teammate.
    #[must_use]
    pub fn new(id: WorkerId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            status: TeammateStatus::Idle,
        }
    }
}

/// A named team: a coordinator plus its named teammates.
///
/// The team itself only tracks membership and per-teammate status; the
/// transition driving (and event emission) lives on [`TeamRegistry`] so the
/// [`MissionLog`] and per-teammate mailboxes stay alongside it.
#[derive(Debug, Clone)]
pub struct Team {
    /// Team name (unique within a [`TeamRegistry`]).
    pub name: String,
    /// The coordinating worker for this team.
    pub coordinator: WorkerId,
    /// Members, in registration order.
    pub teammates: Vec<Teammate>,
}

impl Team {
    /// Construct an empty team led by `coordinator`.
    #[must_use]
    pub fn new(name: impl Into<String>, coordinator: WorkerId) -> Self {
        Self {
            name: name.into(),
            coordinator,
            teammates: Vec::new(),
        }
    }

    /// Register a teammate.
    ///
    /// If a teammate with the same `name` already exists it is replaced (so a
    /// re-spawn under the same name updates the id/status rather than
    /// duplicating the entry). Returns the [`WorkerId`] of the registered
    /// teammate for convenience.
    pub fn register(&mut self, teammate: Teammate) -> WorkerId {
        let id = teammate.id;
        if let Some(existing) = self.teammates.iter_mut().find(|t| t.name == teammate.name) {
            *existing = teammate;
        } else {
            self.teammates.push(teammate);
        }
        id
    }

    /// Register a fresh idle teammate by name; convenience over [`Self::register`].
    pub fn register_named(&mut self, id: WorkerId, name: impl Into<String>) -> WorkerId {
        self.register(Teammate::new(id, name))
    }

    /// Look a teammate up by name (shared borrow).
    #[must_use]
    pub fn teammate(&self, name: &str) -> Option<&Teammate> {
        self.teammates.iter().find(|t| t.name == name)
    }

    /// Look a teammate up by name (mutable borrow).
    #[must_use]
    pub fn teammate_mut(&mut self, name: &str) -> Option<&mut Teammate> {
        self.teammates.iter_mut().find(|t| t.name == name)
    }

    /// Look a teammate up by its [`WorkerId`].
    #[must_use]
    pub fn teammate_by_id(&self, id: WorkerId) -> Option<&Teammate> {
        self.teammates.iter().find(|t| t.id == id)
    }

    /// Look a teammate up by its [`WorkerId`] (mutable borrow).
    #[must_use]
    pub fn teammate_by_id_mut(&mut self, id: WorkerId) -> Option<&mut Teammate> {
        self.teammates.iter_mut().find(|t| t.id == id)
    }

    /// All currently [`TeammateStatus::Idle`] teammates, in registration order.
    #[must_use]
    pub fn idle_teammates(&self) -> Vec<&Teammate> {
        self.teammates.iter().filter(|t| t.status.is_idle()).collect()
    }

    /// Number of registered teammates.
    #[must_use]
    pub fn len(&self) -> usize {
        self.teammates.len()
    }

    /// `true` iff the team has no teammates.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.teammates.is_empty()
    }
}

/// A lifecycle event surfaced as a teammate's status changes.
///
/// These mirror the events claude-code surfaces for Agent Teams: a teammate
/// becoming available again, and a teammate finishing its task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamEvent {
    /// A teammate transitioned to [`TeammateStatus::Idle`] (free for work).
    TeammateIdle {
        /// The teammate that is now idle.
        teammate: WorkerId,
    },
    /// A teammate finished its task and reported back.
    TaskCompleted {
        /// The teammate that completed.
        teammate: WorkerId,
        /// A short, prose-free summary of what was reported (e.g. the goal +
        /// terminal status). The full payload lives in the worker's
        /// [`CompletionReport`](crate::CompletionReport).
        report_summary: String,
    },
}

/// The kind of [`MissionEntry`] recorded in a [`MissionLog`].
///
/// Distinct from [`TeamEvent`]: the log captures *assignments* too (which are
/// not surfaced as a [`TeamEvent`]), so it carries its own small kind enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissionEvent {
    /// A teammate was registered into the team.
    Registered,
    /// A teammate was assigned `task`.
    Assigned {
        /// The task description assigned.
        task: String,
    },
    /// A teammate completed its task with `summary`.
    Completed {
        /// Short, prose-free completion summary.
        summary: String,
    },
    /// A teammate returned to idle.
    Idled,
}

impl MissionEvent {
    /// A short, stable label for the event kind (used by [`MissionLog::render`]).
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Assigned { .. } => "assigned",
            Self::Completed { .. } => "completed",
            Self::Idled => "idle",
        }
    }
}

/// One append-only line in a [`MissionLog`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionEntry {
    /// The teammate the entry is about.
    pub teammate: WorkerId,
    /// What happened.
    pub event: MissionEvent,
    /// Optional free-form note the team attached (the teammate's name, the
    /// task text, a summary, …). Empty when there is nothing to add.
    pub note: String,
}

/// An append-only timeline the team writes as work progresses.
///
/// Pure in-memory `Vec<MissionEntry>`; [`Self::render`] produces a plain-text
/// timeline a daemon/cli layer can surface verbatim.
#[derive(Debug, Clone, Default)]
pub struct MissionLog {
    entries: Vec<MissionEntry>,
}

impl MissionLog {
    /// Construct an empty mission log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an entry.
    pub fn record(&mut self, teammate: WorkerId, event: MissionEvent, note: impl Into<String>) {
        self.entries.push(MissionEntry {
            teammate,
            event,
            note: note.into(),
        });
    }

    /// All recorded entries, in order.
    #[must_use]
    pub fn entries(&self) -> &[MissionEntry] {
        self.entries.as_slice()
    }

    /// Number of recorded entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` iff nothing has been recorded yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Render the log as a plain-text timeline, one entry per line.
    ///
    /// Each line is `#<n> [<label>] <worker-id-hex> <note>` so the ordering is
    /// explicit and the worker id is stable across runs. The note is omitted
    /// (no trailing space) when empty.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (i, e) in self.entries.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            // `#<n> [<label>] <id>` then an optional ` <note>` tail.
            let _ = write!(
                out,
                "#{} [{}] {:032x}",
                i + 1,
                e.event.label(),
                e.teammate.value()
            );
            if !e.note.is_empty() {
                out.push(' ');
                out.push_str(&e.note);
            }
        }
        out
    }
}

/// Error from a [`TeamRegistry`] operation that names an unknown team/teammate.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TeamError {
    /// No team with the given name exists.
    #[error("no such team: {0}")]
    NoSuchTeam(String),
    /// No teammate with the given name exists in the team.
    #[error("no such teammate {teammate} in team {team}")]
    NoSuchTeammate {
        /// The team that was looked in.
        team: String,
        /// The teammate name that was not found.
        teammate: String,
    },
}

/// Owns the named teams, their mission logs, and per-teammate mailboxes.
///
/// One registry per coordinator "room". It is the single place that drives
/// status transitions so that the [`MissionLog`] and emitted [`TeamEvent`]s
/// stay consistent with the per-teammate [`TeammateStatus`].
///
/// Mailboxes are keyed by [`WorkerId`] (not by name) so a teammate keeps its
/// inbox across a rename, and reuse the WS-L [`Mailbox`]/[`Message`] types.
#[derive(Debug, Default)]
pub struct TeamRegistry {
    teams: HashMap<String, Team>,
    log: HashMap<String, MissionLog>,
    mailboxes: HashMap<WorkerId, Mailbox>,
}

impl TeamRegistry {
    /// Construct an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create (or replace) a named team led by `coordinator`.
    ///
    /// Any prior team of the same name is overwritten. Returns a mutable borrow
    /// of the freshly created team so callers can immediately register
    /// teammates against it.
    pub fn create_team(&mut self, name: impl Into<String>, coordinator: WorkerId) -> &mut Team {
        use std::collections::hash_map::Entry;
        let name = name.into();
        self.log.insert(name.clone(), MissionLog::new());
        // Overwrite any prior team of the same name with a fresh one so
        // `create_team` is idempotent-by-replace, then hand back the slot.
        match self.teams.entry(name.clone()) {
            Entry::Occupied(mut e) => {
                *e.get_mut() = Team::new(name, coordinator);
                e.into_mut()
            }
            Entry::Vacant(e) => e.insert(Team::new(name, coordinator)),
        }
    }

    /// Look a team up by name.
    #[must_use]
    pub fn team(&self, name: &str) -> Option<&Team> {
        self.teams.get(name)
    }

    /// Look a team up by name (mutable borrow).
    #[must_use]
    pub fn team_mut(&mut self, name: &str) -> Option<&mut Team> {
        self.teams.get_mut(name)
    }

    /// The mission log for a team, if the team exists.
    #[must_use]
    pub fn mission_log(&self, team: &str) -> Option<&MissionLog> {
        self.log.get(team)
    }

    /// Register a teammate into `team` and seed its mailbox.
    ///
    /// Records a [`MissionEvent::Registered`] entry noting the teammate's name.
    ///
    /// # Errors
    /// Returns [`TeamError::NoSuchTeam`] when `team` does not exist.
    pub fn register_teammate(
        &mut self,
        team: &str,
        id: WorkerId,
        name: impl Into<String>,
    ) -> Result<WorkerId, TeamError> {
        let name = name.into();
        let t = self
            .teams
            .get_mut(team)
            .ok_or_else(|| TeamError::NoSuchTeam(team.to_owned()))?;
        t.register_named(id, name.clone());
        self.mailboxes.entry(id).or_default();
        if let Some(log) = self.log.get_mut(team) {
            log.record(id, MissionEvent::Registered, name);
        }
        Ok(id)
    }

    /// Assign `task` to a named teammate.
    ///
    /// Transitions the teammate to [`TeammateStatus::Working`] and records a
    /// [`MissionEvent::Assigned`] entry. No [`TeamEvent`] is emitted for an
    /// assignment. Returns the assigned teammate's [`WorkerId`].
    ///
    /// # Errors
    /// [`TeamError::NoSuchTeam`] / [`TeamError::NoSuchTeammate`] when the team
    /// or teammate is unknown.
    pub fn assign_task(
        &mut self,
        team: &str,
        teammate: &str,
        task: impl Into<String>,
    ) -> Result<WorkerId, TeamError> {
        let task = task.into();
        let id = {
            let mate = self.teammate_mut(team, teammate)?;
            mate.status = TeammateStatus::Working { task: task.clone() };
            mate.id
        };
        if let Some(log) = self.log.get_mut(team) {
            log.record(id, MissionEvent::Assigned { task: task.clone() }, task);
        }
        Ok(id)
    }

    /// Complete a teammate's task.
    ///
    /// Transitions the teammate to [`TeammateStatus::Done`], records a
    /// [`MissionEvent::Completed`] entry, and returns the
    /// [`TeamEvent::TaskCompleted`] event to surface.
    ///
    /// # Errors
    /// [`TeamError::NoSuchTeam`] / [`TeamError::NoSuchTeammate`].
    pub fn complete_task(
        &mut self,
        team: &str,
        teammate: &str,
        report_summary: impl Into<String>,
    ) -> Result<TeamEvent, TeamError> {
        let report_summary = report_summary.into();
        let id = {
            let mate = self.teammate_mut(team, teammate)?;
            mate.status = TeammateStatus::Done;
            mate.id
        };
        if let Some(log) = self.log.get_mut(team) {
            log.record(
                id,
                MissionEvent::Completed {
                    summary: report_summary.clone(),
                },
                report_summary.clone(),
            );
        }
        Ok(TeamEvent::TaskCompleted {
            teammate: id,
            report_summary,
        })
    }

    /// Mark a teammate idle.
    ///
    /// Transitions the teammate to [`TeammateStatus::Idle`], records a
    /// [`MissionEvent::Idled`] entry, and returns the [`TeamEvent::TeammateIdle`]
    /// event to surface.
    ///
    /// # Errors
    /// [`TeamError::NoSuchTeam`] / [`TeamError::NoSuchTeammate`].
    pub fn mark_idle(&mut self, team: &str, teammate: &str) -> Result<TeamEvent, TeamError> {
        let id = {
            let mate = self.teammate_mut(team, teammate)?;
            mate.status = TeammateStatus::Idle;
            mate.id
        };
        if let Some(log) = self.log.get_mut(team) {
            log.record(id, MissionEvent::Idled, String::new());
        }
        Ok(TeamEvent::TeammateIdle { teammate: id })
    }

    /// Send a [`Message`] to a teammate's mailbox (reuses WS-L [`Mailbox`]).
    ///
    /// The message is delivered iff its [`MsgScope`](crate::collab::MsgScope)
    /// `delivers_to` the recipient: a [`Direct`](crate::collab::MsgScope::Direct)
    /// to another teammate is dropped, while `Repo`/`Broadcast` always deliver.
    /// Returns `true` when the message was queued.
    ///
    /// # Errors
    /// [`TeamError::NoSuchTeam`] / [`TeamError::NoSuchTeammate`].
    pub fn send_to_teammate(&mut self, team: &str, teammate: &str, msg: Message) -> Result<bool, TeamError> {
        let id = self.teammate_mut(team, teammate)?.id;
        let delivered = msg.scope.delivers_to(id);
        if delivered {
            self.mailboxes.entry(id).or_default().push(msg);
        }
        Ok(delivered)
    }

    /// Drain a teammate's mailbox in FIFO order (reuses WS-L [`Mailbox`]).
    ///
    /// Returns an empty `Vec` when the teammate has no mailbox or no queued
    /// messages.
    ///
    /// # Errors
    /// [`TeamError::NoSuchTeam`] / [`TeamError::NoSuchTeammate`].
    pub fn drain_teammate_inbox(&mut self, team: &str, teammate: &str) -> Result<Vec<Message>, TeamError> {
        let id = self.teammate_mut(team, teammate)?.id;
        Ok(self.mailboxes.get(&id).map(Mailbox::drain).unwrap_or_default())
    }

    /// Shared helper: resolve `(team, teammate)` to a mutable teammate ref.
    fn teammate_mut(&mut self, team: &str, teammate: &str) -> Result<&mut Teammate, TeamError> {
        let t = self
            .teams
            .get_mut(team)
            .ok_or_else(|| TeamError::NoSuchTeam(team.to_owned()))?;
        t.teammate_mut(teammate).ok_or_else(|| TeamError::NoSuchTeammate {
            team: team.to_owned(),
            teammate: teammate.to_owned(),
        })
    }
}

/// Build a short, prose-free summary line for a finished worker's report.
///
/// Convenience the daemon/cli layer can use when turning a
/// [`CompletionReport`](crate::CompletionReport) into a
/// [`TeamEvent::TaskCompleted`] summary, keeping the team module the single
/// place that defines the summary shape. Pure formatting; no allocation beyond
/// the returned `String`.
#[must_use]
pub fn report_summary(report: &crate::report::CompletionReport) -> String {
    format!(
        "{:?}: {} ({} plan-ops, {} files)",
        report.status,
        report.goal,
        report.plan_updates.len(),
        report.files_touched.len(),
    )
}

#[cfg(test)]
#[allow(clippy::panic)] // assertion macros + test invariants may panic/unreachable.
mod tests {
    use super::*;
    use crate::collab::MsgScope;
    use crate::coordinator::WorkerId;

    fn registry_with_team(name: &str) -> (TeamRegistry, WorkerId) {
        let mut reg = TeamRegistry::new();
        let coord = WorkerId::generate();
        reg.create_team(name, coord);
        (reg, coord)
    }

    // ── Team membership ───────────────────────────────────────────────────

    #[test]
    fn register_and_lookup_by_name() {
        let coord = WorkerId::generate();
        let mut team = Team::new("alpha", coord);
        let w = WorkerId::generate();

        team.register_named(w, "scout");

        let found = team.teammate("scout").expect("registered");
        assert_eq!(found.id, w);
        assert_eq!(found.name, "scout");
        assert!(found.status.is_idle());
        assert!(team.teammate("ghost").is_none());
    }

    #[test]
    fn register_same_name_replaces_not_duplicates() {
        let mut team = Team::new("alpha", WorkerId::generate());
        let first = WorkerId::generate();
        let second = WorkerId::generate();

        team.register_named(first, "scout");
        team.register_named(second, "scout"); // re-spawn under same name

        assert_eq!(team.len(), 1, "same name must replace, not duplicate");
        assert_eq!(team.teammate("scout").expect("present").id, second);
    }

    #[test]
    fn idle_detection_lists_only_idle() {
        let mut team = Team::new("alpha", WorkerId::generate());
        let a = WorkerId::generate();
        let b = WorkerId::generate();
        team.register_named(a, "a");
        team.register_named(b, "b");

        // Put `a` to work; only `b` should remain idle.
        team.teammate_mut("a").expect("a").status = TeammateStatus::Working { task: "x".into() };

        let idle: Vec<&str> = team.idle_teammates().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(idle, vec!["b"]);
    }

    #[test]
    fn lookup_by_id() {
        let mut team = Team::new("alpha", WorkerId::generate());
        let w = WorkerId::generate();
        team.register_named(w, "scout");
        assert_eq!(team.teammate_by_id(w).expect("by id").name, "scout");
        assert!(team.teammate_by_id(WorkerId::generate()).is_none());
    }

    #[test]
    fn empty_team_is_empty() {
        let team = Team::new("alpha", WorkerId::generate());
        assert!(team.is_empty());
        assert_eq!(team.len(), 0);
        assert!(team.idle_teammates().is_empty());
    }

    // ── Status enum ───────────────────────────────────────────────────────

    #[test]
    fn status_predicates() {
        assert!(TeammateStatus::Idle.is_idle());
        assert!(TeammateStatus::default().is_idle());
        assert!(TeammateStatus::Working { task: "t".into() }.is_working());
        assert!(TeammateStatus::Done.is_done());
        assert!(!TeammateStatus::Done.is_idle());
    }

    // ── TeamRegistry transitions ──────────────────────────────────────────

    #[test]
    fn create_and_lookup_team() {
        let (reg, coord) = registry_with_team("alpha");
        let t = reg.team("alpha").expect("created");
        assert_eq!(t.name, "alpha");
        assert_eq!(t.coordinator, coord);
        assert!(reg.team("missing").is_none());
    }

    #[test]
    fn register_teammate_records_mission_entry() {
        let (mut reg, _) = registry_with_team("alpha");
        let w = WorkerId::generate();
        reg.register_teammate("alpha", w, "scout").expect("team exists");

        let log = reg.mission_log("alpha").expect("log");
        assert_eq!(log.len(), 1);
        let e = &log.entries()[0];
        assert_eq!(e.teammate, w);
        assert_eq!(e.event, MissionEvent::Registered);
        assert_eq!(e.note, "scout");
    }

    #[test]
    fn register_teammate_unknown_team_errors() {
        let mut reg = TeamRegistry::new();
        let err = reg
            .register_teammate("ghost", WorkerId::generate(), "scout")
            .expect_err("no such team");
        assert_eq!(err, TeamError::NoSuchTeam("ghost".into()));
    }

    #[test]
    fn assign_then_complete_emits_event_and_log_entries() {
        let (mut reg, _) = registry_with_team("alpha");
        let w = WorkerId::generate();
        reg.register_teammate("alpha", w, "scout").expect("team");

        let assigned_id = reg
            .assign_task("alpha", "scout", "survey the repo")
            .expect("assign");
        assert_eq!(assigned_id, w);
        // Status flipped to Working { task }.
        assert_eq!(
            reg.team("alpha")
                .expect("team")
                .teammate("scout")
                .expect("mate")
                .status,
            TeammateStatus::Working {
                task: "survey the repo".into()
            }
        );

        let event = reg
            .complete_task("alpha", "scout", "found 3 crates")
            .expect("complete");
        assert_eq!(
            event,
            TeamEvent::TaskCompleted {
                teammate: w,
                report_summary: "found 3 crates".into()
            }
        );
        // Status flipped to Done.
        assert!(reg
            .team("alpha")
            .expect("team")
            .teammate("scout")
            .expect("mate")
            .status
            .is_done());

        // Mission log: registered, assigned, completed.
        let log = reg.mission_log("alpha").expect("log");
        assert_eq!(log.len(), 3);
        assert_eq!(
            log.entries()[1].event,
            MissionEvent::Assigned {
                task: "survey the repo".into()
            }
        );
        assert_eq!(
            log.entries()[2].event,
            MissionEvent::Completed {
                summary: "found 3 crates".into()
            }
        );
    }

    #[test]
    fn mark_idle_emits_teammate_idle_and_logs() {
        let (mut reg, _) = registry_with_team("alpha");
        let w = WorkerId::generate();
        reg.register_teammate("alpha", w, "scout").expect("team");
        reg.assign_task("alpha", "scout", "task").expect("assign");

        let event = reg.mark_idle("alpha", "scout").expect("idle");
        assert_eq!(event, TeamEvent::TeammateIdle { teammate: w });
        assert!(reg
            .team("alpha")
            .expect("team")
            .teammate("scout")
            .expect("mate")
            .status
            .is_idle());

        // registered, assigned, idle.
        let log = reg.mission_log("alpha").expect("log");
        assert_eq!(log.entries().last().expect("last").event, MissionEvent::Idled);
    }

    #[test]
    fn assign_unknown_teammate_errors() {
        let (mut reg, _) = registry_with_team("alpha");
        let err = reg
            .assign_task("alpha", "ghost", "t")
            .expect_err("no such teammate");
        assert_eq!(
            err,
            TeamError::NoSuchTeammate {
                team: "alpha".into(),
                teammate: "ghost".into()
            }
        );
    }

    // ── MissionLog ────────────────────────────────────────────────────────

    #[test]
    fn mission_log_render_is_ordered_timeline() {
        let mut log = MissionLog::new();
        let w = WorkerId::generate();
        log.record(w, MissionEvent::Registered, "scout");
        log.record(w, MissionEvent::Assigned { task: "t".into() }, "t");
        log.record(w, MissionEvent::Idled, "");

        let rendered = log.render();
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("#1 [registered] "));
        assert!(lines[0].ends_with(" scout"));
        assert!(lines[1].starts_with("#2 [assigned] "));
        assert!(lines[1].ends_with(" t"));
        // Empty note ⇒ no trailing space.
        assert!(lines[2].starts_with("#3 [idle] "));
        assert!(!lines[2].ends_with(' '));
    }

    #[test]
    fn mission_log_render_empty_is_empty_string() {
        assert_eq!(MissionLog::new().render(), "");
        assert!(MissionLog::new().is_empty());
    }

    // ── Mailbox (reuse of WS-L collab) ─────────────────────────────────────

    #[test]
    fn mailbox_message_to_a_teammate_round_trips() {
        let (mut reg, coord) = registry_with_team("alpha");
        let w = WorkerId::generate();
        reg.register_teammate("alpha", w, "scout").expect("team");

        // Coordinator DMs the teammate.
        let msg = Message::new(coord, MsgScope::Direct(w), "ping");
        let delivered = reg.send_to_teammate("alpha", "scout", msg.clone()).expect("send");
        assert!(delivered, "a Direct message to the recipient must be delivered");

        let drained = reg.drain_teammate_inbox("alpha", "scout").expect("drain");
        assert_eq!(drained, vec![msg]);

        // Draining again yields nothing.
        assert!(reg
            .drain_teammate_inbox("alpha", "scout")
            .expect("drain2")
            .is_empty());
    }

    #[test]
    fn direct_message_to_other_is_not_delivered() {
        let (mut reg, coord) = registry_with_team("alpha");
        let w = WorkerId::generate();
        let other = WorkerId::generate();
        reg.register_teammate("alpha", w, "scout").expect("team");

        // A Direct message addressed to a different worker must not land in scout's box.
        let msg = Message::new(coord, MsgScope::Direct(other), "not for you");
        let delivered = reg.send_to_teammate("alpha", "scout", msg).expect("send");
        assert!(!delivered);
        assert!(reg
            .drain_teammate_inbox("alpha", "scout")
            .expect("drain")
            .is_empty());
    }

    #[test]
    fn broadcast_message_delivers_to_teammate() {
        let (mut reg, coord) = registry_with_team("alpha");
        let w = WorkerId::generate();
        reg.register_teammate("alpha", w, "scout").expect("team");

        let msg = Message::new(coord, MsgScope::Broadcast, "all hands");
        assert!(reg.send_to_teammate("alpha", "scout", msg).expect("send"));
        assert_eq!(
            reg.drain_teammate_inbox("alpha", "scout").expect("drain").len(),
            1
        );
    }

    // ── report_summary helper ──────────────────────────────────────────────

    #[test]
    fn report_summary_is_prose_free_line() {
        use crate::report::CompletionReport;
        use crate::spec::{ReportStatus, Usage};

        let report = CompletionReport {
            goal: "build the thing".into(),
            status: ReportStatus::Completed,
            plan_updates: Vec::new(),
            files_touched: vec![[0u8; 32]],
            decisions: Vec::new(),
            follow_ups: Vec::new(),
            transcript_handle: [0u8; 32],
            usage: Usage::default(),
        };
        let summary = report_summary(&report);
        assert!(summary.contains("Completed"));
        assert!(summary.contains("build the thing"));
        assert!(summary.contains("0 plan-ops"));
        assert!(summary.contains("1 files"));
    }
}
