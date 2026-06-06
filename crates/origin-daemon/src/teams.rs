// SPDX-License-Identifier: Apache-2.0
//! Daemon-side control plane for named agent teams.
//!
//! The named-teammate vocabulary + bookkeeping lives in
//! [`origin_swarm::team`]; this module is the daemon's thin adapter that holds a
//! process-global [`TeamRegistry`] and drives it from IPC `Team*` verbs, spawning
//! a REAL swarm worker (via the daemon's live [`Coordinator`](origin_swarm::Coordinator))
//! as a named teammate on assign.
//!
//! # Default-off by construction
//!
//! No team exists unless a client sends `TeamCreate`; the registry starts empty
//! and is never touched otherwise, so default daemon behaviour is byte-identical.
//! The registry is created lazily on first `TeamCreate`, mirroring the ambient
//! `IDLE_TRACKER` / [`crate::supervisor`] `STATE` `OnceLock` pattern.
//!
//! # What is wired vs. deferred
//!
//! - `TeamCreate` / `TeamStatus` / the `MissionLog` render / the per-teammate
//!   status transitions and the [`TeamEvent`](origin_swarm::TeamEvent) bridge are
//!   fully wired.
//! - `TeamAssign` spawns a real worker through the coordinator's `spawn`/
//!   `await_completion` substrate (the same one Stage 4 installed `real_worker`
//!   on), transitions the teammate Working → Done → Idle, and journals + bridges
//!   each event. The worker runs on `TaskClass::Swarm` exactly like a `Task`
//!   sub-agent.

use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use origin_plan::ActorId;
use origin_swarm::{
    report_summary, Budget, Coordinator, TeamError, TeamEvent, TeamRegistry, TeammateStatus, WorkerId,
    WorkerSpec,
};

use crate::protocol::StreamEvent;

/// Default tool allow-list for a spawned teammate. Deliberately read-only +
/// search builtins so a teammate cannot mutate the tree unless a later policy
/// opts it in; `Task` is always stripped by the worker substrate.
const DEFAULT_TEAMMATE_TOOLS: &[&str] = &["Read", "Grep", "Glob"];

/// Default per-teammate budget. Mirrors the `Task` sub-agent default tool-call
/// cap; the wall/token ceilings are generous since a teammate is a full turn.
const TEAMMATE_BUDGET: Budget = Budget::new(
    /* max_wall_ms */ 300_000, /* max_input_tokens */ 1_000_000,
    /* max_output_tokens */ 256_000, /* max_tool_calls */ 32,
);

/// Process-global team registry. `None` until the first `TeamCreate`; an empty
/// registry otherwise, so the default path constructs nothing.
static REGISTRY: OnceLock<Mutex<TeamRegistry>> = OnceLock::new();

/// Lock the process-global registry, recovering from poisoning.
fn registry() -> MutexGuard<'static, TeamRegistry> {
    REGISTRY
        .get_or_init(|| Mutex::new(TeamRegistry::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Create (or replace) a named team and return its initial status event.
///
/// A fresh team has an empty `MissionLog`, so the returned [`StreamEvent::TeamStatus`]
/// reflects an empty timeline + no teammates. Idempotent-by-replace.
#[must_use]
pub fn create_team(name: &str) -> StreamEvent {
    {
        let mut reg = registry();
        // The team's coordinator id is a synthetic, stable id; the real worker
        // substrate is the daemon-wide `Coordinator`, so this is bookkeeping
        // only (it labels the team's lead in the MissionLog vocabulary).
        reg.create_team(name, WorkerId::generate());
    }
    status_event(name).unwrap_or_else(|| StreamEvent::TeamStatus {
        team: name.to_string(),
        mission_log: String::new(),
        teammates: Vec::new(),
    })
}

/// Render a team's `MissionLog` + per-teammate statuses, or `None` if unknown.
#[must_use]
// The guard spans the team read + the MissionLog render + the teammate fold;
// the `?` early-returns `None` for an unknown team. This is the minimum scope.
#[allow(clippy::significant_drop_tightening)]
pub fn status_event(team: &str) -> Option<StreamEvent> {
    let reg = registry();
    let t = reg.team(team)?;
    let mission_log = reg
        .mission_log(team)
        .map(origin_swarm::MissionLog::render)
        .unwrap_or_default();
    let teammates: Vec<String> = t.teammates.iter().map(render_teammate).collect();
    Some(StreamEvent::TeamStatus {
        team: team.to_string(),
        mission_log,
        teammates,
    })
}

/// One `name: status` status line for a teammate.
fn render_teammate(mate: &origin_swarm::Teammate) -> String {
    let status = match &mate.status {
        TeammateStatus::Idle => "idle".to_string(),
        TeammateStatus::Working { task } => format!("working: {task}"),
        TeammateStatus::Done => "done".to_string(),
    };
    format!("{}: {status}", mate.name)
}

/// Register a teammate (idempotent-by-name) and mark it `Working` on `task`.
///
/// Returns the teammate's real [`WorkerId`] for the spawn, or a [`TeamError`]
/// when the team is unknown. The teammate is registered fresh against a real
/// worker id so the `MissionLog` and the spawned worker share an identity.
///
/// # Errors
/// [`TeamError::NoSuchTeam`] when `team` was never created.
// The registry guard must span register + assign so the two mutations are
// atomic; tightening it further is not possible.
#[allow(clippy::significant_drop_tightening)]
pub fn begin_assignment(team: &str, teammate: &str, task: &str) -> Result<WorkerId, TeamError> {
    let mut reg = registry();
    // A real worker id identifies this teammate across the registry, the
    // MissionLog, and the spawned worker. Registering is idempotent-by-name
    // (a re-assign reuses the slot), then we flip it to Working.
    let id = WorkerId::generate();
    reg.register_teammate(team, id, teammate)?;
    reg.assign_task(team, teammate, task)?;
    Ok(id)
}

/// Build the [`WorkerSpec`] for a teammate pursuing `task`.
#[must_use]
fn teammate_spec(task: &str) -> WorkerSpec {
    WorkerSpec {
        goal: task.to_string(),
        allowed_tools: DEFAULT_TEAMMATE_TOOLS.iter().map(|s| (*s).to_string()).collect(),
        budget: TEAMMATE_BUDGET,
        workspace: None,
        parent_actor: ActorId::new(0),
        model: None,
        mcp_servers: Vec::new(),
    }
}

/// Spawn a real swarm worker as `teammate` and drive its lifecycle to completion.
///
/// On the worker's terminal report this transitions the teammate
/// `Working → Done` (emitting [`TeamEvent::TaskCompleted`]) then `Done → Idle`
/// (emitting [`TeamEvent::TeammateIdle`]), journaling each to the team's
/// `MissionLog`. Both events are returned so the caller can bridge them onto the
/// wire + lifecycle hooks. Best-effort: a spawn/await failure still settles the
/// teammate to Done (with the error as the summary) so the team never wedges.
// The registry guard must span complete + mark-idle so both transitions and
// their MissionLog entries are atomic; it is acquired only AFTER the awaits.
#[allow(clippy::significant_drop_tightening)]
pub async fn run_teammate(
    coordinator: &Coordinator,
    team: &str,
    teammate: &str,
    task: &str,
) -> Vec<TeamEvent> {
    let spec = teammate_spec(task);
    let summary = match coordinator.spawn(spec).await {
        Ok(handle) => match coordinator.await_completion(&handle).await {
            Ok(report) => report_summary(&report),
            Err(e) => format!("worker failed: {e}"),
        },
        Err(e) => format!("spawn failed: {e}"),
    };

    let mut events = Vec::with_capacity(2);
    let mut reg = registry();
    // Complete (Working → Done) then mark idle (Done → Idle), each journaled to
    // the MissionLog by the registry. A NoSuchTeammate here means the team was
    // removed mid-flight; we drop the events rather than error.
    if let Ok(ev) = reg.complete_task(team, teammate, summary) {
        events.push(ev);
    }
    if let Ok(ev) = reg.mark_idle(team, teammate) {
        events.push(ev);
    }
    events
}

/// Bridge a [`TeamEvent`] onto the wire as a [`StreamEvent::TeamEventFired`].
#[must_use]
pub fn event_to_stream(team: &str, event: &TeamEvent) -> StreamEvent {
    match event {
        TeamEvent::TeammateIdle { teammate } => StreamEvent::TeamEventFired {
            team: team.to_string(),
            event_kind: "teammate_idle".to_string(),
            teammate: format!("{:032x}", teammate.value()),
            summary: String::new(),
        },
        TeamEvent::TaskCompleted {
            teammate,
            report_summary,
        } => StreamEvent::TeamEventFired {
            team: team.to_string(),
            event_kind: "task_completed".to_string(),
            teammate: format!("{:032x}", teammate.value()),
            summary: report_summary.clone(),
        },
    }
}

/// A short, human-readable lifecycle-hook message for a [`TeamEvent`].
///
/// Bridged to [`crate::hooks_runtime::fire_global`] as a
/// [`Notification`](origin_hooks::LifecycleEvent::Notification) so a configured
/// `hooks.json` can observe teammate lifecycle. A no-op without hooks configured.
#[must_use]
pub fn event_to_notification(team: &str, event: &TeamEvent) -> String {
    match event {
        TeamEvent::TeammateIdle { teammate } => {
            format!("team {team}: teammate {:032x} idle", teammate.value())
        }
        TeamEvent::TaskCompleted {
            teammate,
            report_summary,
        } => format!(
            "team {team}: teammate {:032x} completed — {report_summary}",
            teammate.value()
        ),
    }
}

/// Whether a team exists (used by the handler to choose `AdminError` vs status).
#[must_use]
pub fn team_exists(team: &str) -> bool {
    registry().team(team).is_some()
}

/// Test-only: snapshot a clone of a team for assertions.
#[cfg(test)]
#[must_use]
fn team_snapshot(team: &str) -> Option<origin_swarm::Team> {
    registry().team(team).cloned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    // The registry is process-global by design (one per daemon), so tests must
    // not wipe it (that would race other parallel tests). Instead each test uses
    // a unique team name so cases never collide.
    fn unique(prefix: &str) -> String {
        format!("{prefix}-{}", ulid::Ulid::new())
    }

    #[test]
    fn create_then_status_is_empty_team() {
        let name = unique("alpha");
        let ev = create_team(&name);
        match ev {
            StreamEvent::TeamStatus {
                team,
                mission_log,
                teammates,
            } => {
                assert_eq!(team, name);
                assert!(mission_log.is_empty(), "fresh team has empty log");
                assert!(teammates.is_empty());
            }
            other => panic!("expected TeamStatus, got {other:?}"),
        }
        assert!(team_exists(&name));
    }

    #[test]
    fn begin_assignment_marks_working_and_journals() {
        let name = unique("beta");
        let _ = create_team(&name);
        let id = begin_assignment(&name, "scout", "survey the repo").unwrap();

        let team = team_snapshot(&name).unwrap();
        let mate = team.teammate("scout").unwrap();
        assert_eq!(mate.id, id);
        assert_eq!(
            mate.status,
            TeammateStatus::Working {
                task: "survey the repo".into()
            }
        );
        // MissionLog recorded registered + assigned.
        let status = status_event(&name).unwrap();
        match status {
            StreamEvent::TeamStatus {
                mission_log,
                teammates,
                ..
            } => {
                assert!(mission_log.contains("[registered]"));
                assert!(mission_log.contains("[assigned]"));
                assert_eq!(teammates, vec!["scout: working: survey the repo".to_string()]);
            }
            other => panic!("expected TeamStatus, got {other:?}"),
        }
    }

    #[test]
    fn begin_assignment_unknown_team_errors() {
        // A guaranteed-unique name that is never created.
        let err = begin_assignment(&unique("ghost"), "scout", "t").unwrap_err();
        assert!(matches!(err, TeamError::NoSuchTeam(_)));
    }

    #[test]
    fn status_unknown_team_is_none() {
        let name = unique("nope");
        assert!(status_event(&name).is_none());
        assert!(!team_exists(&name));
    }

    #[test]
    fn event_bridges_to_stream_and_notification() {
        let w = WorkerId::generate();
        let completed = TeamEvent::TaskCompleted {
            teammate: w,
            report_summary: "did the thing".into(),
        };
        let ev = event_to_stream("gamma", &completed);
        match ev {
            StreamEvent::TeamEventFired {
                team,
                event_kind,
                teammate,
                summary,
            } => {
                assert_eq!(team, "gamma");
                assert_eq!(event_kind, "task_completed");
                assert_eq!(teammate, format!("{:032x}", w.value()));
                assert_eq!(summary, "did the thing");
            }
            other => panic!("expected TeamEventFired, got {other:?}"),
        }
        let note = event_to_notification("gamma", &completed);
        assert!(note.contains("completed"));
        assert!(note.contains("did the thing"));

        let idle = TeamEvent::TeammateIdle { teammate: w };
        match event_to_stream("gamma", &idle) {
            StreamEvent::TeamEventFired {
                event_kind, summary, ..
            } => {
                assert_eq!(event_kind, "teammate_idle");
                assert!(summary.is_empty());
            }
            other => panic!("expected TeamEventFired, got {other:?}"),
        }
    }

    #[test]
    fn teammate_spec_strips_nothing_but_uses_defaults() {
        let spec = teammate_spec("go");
        assert_eq!(spec.goal, "go");
        assert_eq!(spec.allowed_tools, vec!["Read", "Grep", "Glob"]);
        assert_eq!(spec.budget.max_tool_calls, 32);
        assert!(spec.model.is_none());
    }
}
