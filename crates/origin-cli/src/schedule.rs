// SPDX-License-Identifier: Apache-2.0
//! `origin schedule` — manage recurring triggers (cron / `@every` / `@daily` /
//! webhook / fs-event) persisted to `~/.origin/schedule.toml`.
//!
//! Spec parsing and next-fire computation come from [`origin_schedule`]. The
//! daemon reads the same file to actually fire triggers; this CLI surface is
//! the management front-end (add / list / remove).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::cli_def::ScheduleSub;

/// One persisted trigger row.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TriggerEntry {
    id: String,
    spec: String,
    prompt: String,
}

/// On-disk schedule file.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ScheduleFile {
    #[serde(default)]
    triggers: Vec<TriggerEntry>,
}

/// Dispatch a `schedule` subcommand.
///
/// # Errors
/// Returns on filesystem / TOML failure or on an invalid schedule spec.
pub fn run(sub: ScheduleSub) -> Result<()> {
    match sub {
        ScheduleSub::Add { id, spec, prompt } => add(id, spec, prompt),
        ScheduleSub::Ls => list(),
        ScheduleSub::Rm { id } => remove(&id),
    }
}

fn store_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let dir = home.join(".origin");
    std::fs::create_dir_all(&dir).map_err(|e| anyhow::anyhow!("creating {}: {e}", dir.display()))?;
    Ok(dir.join("schedule.toml"))
}

fn load() -> Result<ScheduleFile> {
    let path = store_path()?;
    match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s).map_err(|e| anyhow::anyhow!("parsing schedule.toml: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ScheduleFile::default()),
        Err(e) => Err(anyhow::anyhow!("reading schedule.toml: {e}")),
    }
}

fn save(f: &ScheduleFile) -> Result<()> {
    let path = store_path()?;
    let body = toml::to_string_pretty(f).map_err(|e| anyhow::anyhow!("serializing schedule.toml: {e}"))?;
    std::fs::write(&path, body).map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
    Ok(())
}

fn add(id: String, spec: String, prompt: String) -> Result<()> {
    // Validate the spec up front so bad triggers never reach the daemon.
    origin_schedule::parse_schedule(&spec)
        .map_err(|e| anyhow::anyhow!("invalid schedule spec {spec:?}: {e}"))?;
    let mut f = load()?;
    if f.triggers.iter().any(|t| t.id == id) {
        anyhow::bail!("a trigger with id `{id}` already exists");
    }
    println!("added trigger `{id}`");
    f.triggers.push(TriggerEntry { id, spec, prompt });
    save(&f)?;
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn list() -> Result<()> {
    let f = load()?;
    if f.triggers.is_empty() {
        println!("no scheduled triggers");
        return Ok(());
    }
    let now = now_ms();
    println!("{:<16} {:<20} {:<24} PROMPT", "ID", "SPEC", "NEXT FIRE (Δs)");
    for t in &f.triggers {
        let next = origin_schedule::parse_schedule(&t.spec)
            .ok()
            .and_then(|s| s.next_after(now))
            .map_or_else(
                || "—".to_string(),
                |at| format!("+{}s", at.saturating_sub(now) / 1000),
            );
        println!("{:<16} {:<20} {:<24} {}", t.id, t.spec, next, t.prompt);
    }
    Ok(())
}

fn remove(id: &str) -> Result<()> {
    let mut f = load()?;
    let before = f.triggers.len();
    f.triggers.retain(|t| t.id != id);
    if f.triggers.len() == before {
        println!("no such trigger: `{id}`");
    } else {
        save(&f)?;
        println!("removed trigger `{id}`");
    }
    Ok(())
}
