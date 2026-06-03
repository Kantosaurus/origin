// SPDX-License-Identifier: Apache-2.0
//! Stage-6 subcommand handlers: `origin gmail`, `origin workflow author`,
//! `origin selfdev`, and `origin team`.
//!
//! The daemon-talking handlers (`selfdev` / `team`) mirror the existing admin
//! transport (`crate::admin::round_trip` + a small multi-frame drain): open a
//! one-shot local-socket connection at `$ORIGIN_SOCK` (platform default
//! otherwise), send one [`ClientMessage`], and render the resulting
//! [`StreamEvent`]s. The `gmail` and `workflow` handlers are local: `gmail`
//! calls `origin_gmail::run_tool` directly (it loads credentials from the
//! keyvault), and `workflow author` builds a skill catalog locally and runs the
//! offline `origin_workflowgen` planner, persisting the result through the
//! existing [`crate::workflows`] save path.

use std::time::Duration;

use anyhow::Result;
use origin_daemon::protocol::{ClientMessage, StreamEvent};
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::Connector;

use crate::cli_def::{SelfdevSub, TeamSub, WorkflowSub};

/// `origin gmail <op> [--query …] [--id …] [--max …] [--include-body]`.
///
/// Builds [`origin_gmail::GmailArgs`] from the parsed flags and runs the Gmail
/// tool directly (it loads OAuth credentials from the keyvault). The tool
/// returns a JSON string, which we print verbatim.
///
/// # Errors
/// Forwards any [`origin_gmail::Error`] (bad args, credential, HTTP, or parse
/// failure) as an [`anyhow::Error`].
pub async fn gmail(
    op: String,
    query: Option<String>,
    id: Option<String>,
    max: Option<u32>,
    include_body: bool,
) -> Result<()> {
    // `GmailArgs` is `#[non_exhaustive]` with no public constructor, so build it
    // through serde from the parsed CLI flags (the same shape `from_value` uses).
    let mut obj = serde_json::Map::new();
    obj.insert("op".to_string(), serde_json::Value::String(op));
    if let Some(q) = query {
        obj.insert("query".to_string(), serde_json::Value::String(q));
    }
    if let Some(i) = id {
        obj.insert("id".to_string(), serde_json::Value::String(i));
    }
    if let Some(m) = max {
        obj.insert("max".to_string(), serde_json::Value::Number(m.into()));
    }
    if include_body {
        obj.insert("include_body".to_string(), serde_json::Value::Bool(true));
    }
    let args = origin_gmail::GmailArgs::from_value(&serde_json::Value::Object(obj))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let json = origin_gmail::run_tool(args).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("{json}");
    Ok(())
}

/// `origin workflow author <goal…> [--name <name>]`.
///
/// Builds a skill catalog from the same source the composer's `/` autocomplete
/// uses (embedded `superpowers` skills merged with `~/.origin/skills/`), runs
/// the offline `origin_workflowgen` planner, prints the rendered TOML, and
/// persists the authored workflow into `~/.origin/workflows.toml` via the
/// existing [`crate::workflows`] save path — so it is immediately runnable via
/// `{workflow:<name>}`.
///
/// # Errors
/// Returns when the catalog is empty, the planner cannot author a workflow, or
/// the workflows file cannot be read/written.
pub fn workflow(sub: WorkflowSub) -> Result<()> {
    match sub {
        WorkflowSub::Author { goal, name } => workflow_author(&goal.join(" "), name),
    }
}

fn workflow_author(goal: &str, name: Option<String>) -> Result<()> {
    let catalog = local_skill_catalog();
    if catalog.is_empty() {
        return Err(anyhow::anyhow!(
            "no skills available to author a workflow from (expected embedded skills)"
        ));
    }
    let (mut spec, _toml) =
        origin_workflowgen::author_and_render(goal, &catalog).map_err(|e| anyhow::anyhow!("{e}"))?;
    // Honor an explicit `--name`, overriding the goal-derived slug.
    if let Some(n) = name {
        let n = n.trim();
        if !n.is_empty() {
            spec.name = n.to_string();
        }
    }

    // Map the authored spec onto the daemon/cli `Workflow` shape and persist it
    // through the EXISTING workflows save path (reuse — no duplicate writer).
    let workflow = spec_to_workflow(&spec);
    let path = crate::workflows::path().map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut file = crate::workflows::load_from(&path)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .unwrap_or_else(|| crate::workflows::WorkflowsFile {
            schema_version: crate::workflows::SCHEMA_VERSION,
            workflows: Vec::new(),
        });
    // Replace any existing workflow of the same name (idempotent re-author).
    file.workflows.retain(|w| w.name != workflow.name);
    file.workflows.push(workflow.clone());
    crate::workflows::save_to(&path, &file).map_err(|e| anyhow::anyhow!("{e}"))?;

    // Render the single authored workflow's TOML for display (the saved file may
    // contain other workflows; show just what was authored).
    let rendered = crate::workflows::WorkflowsFile {
        schema_version: crate::workflows::SCHEMA_VERSION,
        workflows: vec![workflow.clone()],
    };
    match toml::to_string_pretty(&rendered) {
        Ok(text) => print!("{text}"),
        Err(e) => return Err(anyhow::anyhow!("toml render: {e}")),
    }
    println!(
        "\nsaved workflow `{}` to {} — run it with {{workflow:{}}}",
        workflow.name,
        path.display(),
        workflow.name
    );
    Ok(())
}

/// Map a `origin_workflowgen::WorkflowSpec` onto the CLI's persisted `Workflow`.
fn spec_to_workflow(spec: &origin_workflowgen::WorkflowSpec) -> crate::workflows::Workflow {
    crate::workflows::Workflow {
        name: spec.name.clone(),
        description: Some(spec.description.clone()),
        steps: spec
            .steps
            .iter()
            .map(|s| crate::workflows::WorkflowStep {
                skill: s.skill.clone(),
                // The planner stores args as a (possibly empty) string; the CLI
                // shape uses `Option<String>` with empty mapped to `None` so the
                // on-disk form stays clean.
                args: if s.args.is_empty() { None } else { Some(s.args.clone()) },
            })
            .collect(),
    }
}

/// Build a [`origin_workflowgen::SkillCatalog`] from the skills the CLI can see
/// locally — embedded `superpowers` skills merged with any user overrides under
/// `~/.origin/skills/`. Mirrors `crate::autocomplete::load_sources`'s source.
fn local_skill_catalog() -> origin_workflowgen::SkillCatalog {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(std::path::PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let skills_dir = home.join(".origin").join("skills");
    let infos: Vec<origin_workflowgen::SkillInfo> = origin_skills::load_all(&skills_dir)
        .map(|v| {
            v.into_iter()
                .map(|s| origin_workflowgen::SkillInfo::new(s.front.name, s.front.description))
                .collect()
        })
        .unwrap_or_default();
    origin_workflowgen::SkillCatalog::new(infos)
}

/// `origin selfdev <start … | status | approve | reset>`.
///
/// Connects to the daemon and sends the matching `SelfDev*` [`ClientMessage`],
/// then renders the resulting [`StreamEvent::SelfDevStatus`] /
/// [`StreamEvent::SelfDevDisabled`]. When `ORIGIN_SELFDEV` is unset the daemon
/// replies `SelfDevDisabled` with an actionable hint; we also print a local
/// note up-front so the user gets guidance even if the daemon is unreachable.
///
/// # Errors
/// Forwards IPC connect/transport and (de)serialization failures.
pub async fn selfdev(sub: SelfdevSub) -> Result<()> {
    if std::env::var("ORIGIN_SELFDEV").as_deref() != Ok("1") {
        println!(
            "note: self-dev is gated — start the daemon with ORIGIN_SELFDEV=1 to enable it."
        );
    }
    let msg = match sub {
        SelfdevSub::Start { description, path } => ClientMessage::SelfDevStart {
            description: description.join(" "),
            paths: path,
        },
        SelfdevSub::Status => ClientMessage::SelfDevStatus,
        SelfdevSub::Approve => ClientMessage::SelfDevApprove,
        SelfdevSub::Reset => ClientMessage::SelfDevReset,
    };
    let events = send_and_drain(msg).await?;
    if events.is_empty() {
        return Err(anyhow::anyhow!("daemon sent no reply"));
    }
    for ev in events {
        render_selfdev_event(&ev);
    }
    Ok(())
}

fn render_selfdev_event(ev: &StreamEvent) {
    match ev {
        StreamEvent::SelfDevStatus {
            state,
            job_id,
            queued,
            consecutive_failures,
            generation,
            storm_guard_tripped,
        } => {
            println!("self-dev: state={state}");
            if let Some(id) = job_id {
                println!("  job: {id}");
            }
            println!("  queued: {queued}");
            println!("  consecutive_failures: {consecutive_failures}");
            println!("  generation: {generation}");
            println!("  storm_guard_tripped: {storm_guard_tripped}");
        }
        StreamEvent::SelfDevDisabled { message } => {
            println!("self-dev disabled: {message}");
        }
        StreamEvent::AdminError { message } => {
            println!("error: {message}");
        }
        other => {
            println!("unexpected reply: {other:?}");
        }
    }
}

/// `origin team <create <name> | assign <team> <teammate> <task> | status <team>>`.
///
/// Connects to the daemon and sends the matching `Team*` [`ClientMessage`], then
/// renders the resulting [`StreamEvent::TeamStatus`] (mission log + teammate
/// statuses) and any [`StreamEvent::TeamEventFired`] teammate lifecycle events.
///
/// # Errors
/// Forwards IPC connect/transport and (de)serialization failures.
pub async fn team(sub: TeamSub) -> Result<()> {
    let msg = match sub {
        TeamSub::Create { name } => ClientMessage::TeamCreate { name },
        TeamSub::Assign { team, teammate, task } => ClientMessage::TeamAssign {
            team,
            teammate,
            task: task.join(" "),
        },
        TeamSub::Status { team } => ClientMessage::TeamStatus { team },
    };
    let events = send_and_drain(msg).await?;
    if events.is_empty() {
        return Err(anyhow::anyhow!("daemon sent no reply"));
    }
    for ev in events {
        render_team_event(&ev);
    }
    Ok(())
}

fn render_team_event(ev: &StreamEvent) {
    match ev {
        StreamEvent::TeamStatus {
            team,
            mission_log,
            teammates,
        } => {
            println!("team `{team}`");
            if !mission_log.trim().is_empty() {
                println!("--- mission log ---");
                println!("{}", mission_log.trim_end());
            }
            if teammates.is_empty() {
                println!("(no teammates yet)");
            } else {
                println!("--- teammates ---");
                for t in teammates {
                    println!("  {t}");
                }
            }
        }
        StreamEvent::TeamEventFired {
            team,
            event_kind,
            teammate,
            summary,
        } => {
            if summary.is_empty() {
                println!("[{team}] {event_kind}: {teammate}");
            } else {
                println!("[{team}] {event_kind}: {teammate} — {summary}");
            }
        }
        StreamEvent::AdminError { message } => {
            println!("error: {message}");
        }
        other => {
            println!("unexpected reply: {other:?}");
        }
    }
}

/// Per-frame read budget for the multi-frame drain. A teammate-assign or a
/// self-dev start can emit several events (status snapshots, teammate lifecycle
/// transitions) on one connection; we read frames until the daemon stops
/// producing them for this long, or the connection closes — whichever comes
/// first. Bounded so the CLI never blocks indefinitely.
const DRAIN_QUIET: Duration = Duration::from_millis(750);

/// Send one [`ClientMessage`] and collect every [`StreamEvent`] the daemon emits
/// on the connection, stopping at the first quiet period ([`DRAIN_QUIET`]) or
/// connection close. Single-reply verbs return one event; streaming verbs (team
/// assign, self-dev start) return the full burst.
async fn send_and_drain(msg: ClientMessage) -> Result<Vec<StreamEvent>> {
    let path = crate::admin::socket_path();
    let mut c = Connector::connect(&path).await?;
    let body = serde_json::to_vec(&msg)?;
    c.write_raw(&encode(1, FrameKind::Request, &body)).await?;

    let mut events = Vec::new();
    // Read frames while they keep arriving in time and decode as events. The
    // first quiet period ([`DRAIN_QUIET`]), connection close, or a frame that
    // is not a decodable `StreamEvent` ends the drain — we have already
    // collected everything that matters.
    while let Ok(Ok(frame)) = tokio::time::timeout(DRAIN_QUIET, c.read_frame_body()).await {
        match serde_json::from_slice::<StreamEvent>(&frame) {
            Ok(ev) => events.push(ev),
            Err(_) => break,
        }
    }
    Ok(events)
}
