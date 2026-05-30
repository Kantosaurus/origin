// SPDX-License-Identifier: Apache-2.0
//! Default-off background scheduler tick loop (item J).
//!
//! When `ORIGIN_SCHEDULER=1` is set, the daemon spawns a background task that
//! periodically loads `~/.origin/schedule.toml` (the same file the
//! `origin schedule add|ls|rm` CLI manages) and, for every trigger that is due
//! on the current tick, **dispatches the trigger's prompt onto the live agent
//! path** by opening a fresh client connection to the daemon's own IPC socket
//! and submitting a `ClientMessage::Prompt`. Reusing the socket means the fired
//! prompt runs through the exact same provider/tool/permission path as an
//! interactive turn, with no daemon-internal handles threaded into this loop.
//!
//! With the env var unset nothing is spawned, so default daemon behaviour is
//! unchanged. *Closes: claude-code `/schedule`+`/loop`; cline cron; kilocode
//! Triggers; opencode cron (the autonomous-firing wire).*

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

/// One persisted trigger row, mirroring the CLI's `schedule.toml` schema.
#[derive(Debug, Clone, Deserialize)]
struct TriggerEntry {
    id: String,
    spec: String,
    prompt: String,
}

/// On-disk schedule file.
#[derive(Debug, Default, Deserialize)]
struct ScheduleFile {
    #[serde(default)]
    triggers: Vec<TriggerEntry>,
}

/// A trigger that came due on the current tick, paired with the prompt to fire.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DueTrigger {
    id: String,
    prompt: String,
}

/// Interval between scheduler ticks.
const TICK: Duration = Duration::from_secs(30);

/// Spawn the background scheduler loop if `ORIGIN_SCHEDULER=1`.
///
/// `sock_path` is the daemon's own IPC socket/pipe path (the one its `Listener`
/// is bound to); fired triggers connect back to it as ordinary clients.
///
/// Default-off: returns immediately (spawning nothing) when the env var is
/// unset or not exactly `"1"`. The spawned task runs for the life of the
/// process; its handle is intentionally dropped (fire-and-forget background
/// work, like the existing telemetry/notify hooks).
pub fn maybe_spawn(sock_path: String) {
    if std::env::var("ORIGIN_SCHEDULER").as_deref() != Ok("1") {
        return;
    }
    tracing::info!("scheduler: ORIGIN_SCHEDULER=1 — starting background tick loop");
    tokio::spawn(async move {
        run_loop(sock_path).await;
    });
}

/// The tick loop: every [`TICK`], reload the schedule file and dispatch the
/// prompt of every trigger whose next-fire time landed in this tick's window.
async fn run_loop(sock_path: String) {
    let model = std::env::var("ORIGIN_MODEL").unwrap_or_else(|_| "claude-opus-4-7".to_string());
    // `last_tick_ms` is the lower bound of the window we check each tick: a
    // trigger fires when its next-fire time falls in `(last_tick_ms, now_ms]`.
    let mut last_tick_ms = now_ms();
    loop {
        tokio::time::sleep(TICK).await;
        let now = now_ms();
        for due in due_triggers(last_tick_ms, now) {
            tracing::info!(id = %due.id, "scheduler: trigger due — dispatching prompt");
            let session_id = format!("sched-{}", now_ms());
            if let Err(e) = dispatch_prompt(&sock_path, &model, session_id, &due.prompt).await {
                tracing::warn!(id = %due.id, error = %e, "scheduler: dispatch failed");
            }
        }
        last_tick_ms = now;
    }
}

/// Path to `~/.origin/schedule.toml`, if a home directory is resolvable.
fn store_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".origin").join("schedule.toml"))
}

/// Load and parse the schedule file. Returns the default (empty) file on any
/// read/parse failure so a malformed or missing file never crashes the loop.
fn load() -> ScheduleFile {
    let Some(path) = store_path() else {
        return ScheduleFile::default();
    };
    std::fs::read_to_string(&path).map_or_else(
        |_| ScheduleFile::default(),
        |s| {
            toml::from_str(&s).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "scheduler: failed to parse schedule.toml");
                ScheduleFile::default()
            })
        },
    )
}

/// Collect every trigger whose next-fire time lands in `(window_start, now]`.
///
/// Pure given the on-disk file (no dispatch, no I/O beyond the file read) so the
/// due-selection windowing is unit-testable without a runtime or live daemon.
fn due_triggers(window_start: u64, now: u64) -> Vec<DueTrigger> {
    let file = load();
    let mut due = Vec::new();
    for t in &file.triggers {
        let Ok(schedule) = origin_schedule::parse_schedule(&t.spec) else {
            tracing::warn!(id = %t.id, spec = %t.spec, "scheduler: invalid spec; skipping");
            continue;
        };
        if let Some(next) = schedule.next_after(window_start) {
            if next <= now {
                due.push(DueTrigger {
                    id: t.id.clone(),
                    prompt: t.prompt.clone(),
                });
            }
        }
    }
    due
}

/// Open a fresh client connection to the daemon's own IPC socket and submit
/// `prompt` as a `ClientMessage::Prompt`, then drain the response stream to
/// completion. Best-effort: any transport error is returned to the caller for
/// logging without crashing the tick loop. Shared with the ambient loop so a
/// fired trigger / ambient task runs through the real agent path.
pub(crate) async fn dispatch_prompt(
    sock_path: &str,
    model: &str,
    session_id: String,
    prompt: &str,
) -> Result<(), String> {
    use origin_ipc::frame::{encode, FrameKind};
    use origin_ipc::transport::Connector;

    let mut conn = Connector::connect(sock_path).await.map_err(|e| e.to_string())?;
    let body = serde_json::to_vec(&crate::protocol::ClientMessage::prompt(
        crate::protocol::PromptRequest {
            system: String::new(),
            model: model.to_string(),
            user_text: prompt.to_string(),
            session_id: Some(session_id),
            ..Default::default()
        },
    ))
    .map_err(|e| e.to_string())?;
    conn.write_raw(&encode(1, FrameKind::Request, &body))
        .await
        .map_err(|e| e.to_string())?;

    // Drain frames until the terminal (non-`StreamEvent`) reply frame arrives,
    // mirroring the headless `origin run` drain loop. An error frame surfaces
    // the loop/provider failure; anything that is not a streaming event is the
    // terminal `PromptReply`.
    loop {
        // Connection closed ⇒ turn finished (or daemon shut down).
        let Ok((kind, frame)) = conn.read_frame().await else {
            break;
        };
        if matches!(kind, FrameKind::ErrorFrame) {
            return Err(String::from_utf8_lossy(&frame).into_owned());
        }
        if serde_json::from_slice::<crate::protocol::StreamEvent>(&frame).is_ok() {
            continue;
        }
        break;
    }
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
    use super::{due_triggers, ScheduleFile};

    #[test]
    fn empty_schedule_fires_nothing() {
        // No schedule.toml in the test home → load() yields the empty default,
        // so no trigger is ever due regardless of the window.
        let due = due_triggers(0, u64::MAX);
        assert!(due.is_empty());
    }

    #[test]
    fn schedule_file_defaults_to_no_triggers() {
        let f = ScheduleFile::default();
        assert!(f.triggers.is_empty());
    }

    #[test]
    fn schedule_file_parses_trigger_rows() {
        let toml = "[[triggers]]\nid = \"nightly\"\nspec = \"@daily 03:00\"\nprompt = \"run tests\"\n";
        let f: ScheduleFile = toml::from_str(toml).expect("parse");
        assert_eq!(f.triggers.len(), 1);
        assert_eq!(f.triggers[0].id, "nightly");
        assert_eq!(f.triggers[0].spec, "@daily 03:00");
        assert_eq!(f.triggers[0].prompt, "run tests");
    }
}
