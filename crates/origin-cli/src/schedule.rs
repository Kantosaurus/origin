// SPDX-License-Identifier: Apache-2.0
//! `origin schedule` — manage recurring triggers (cron / `@every` / `@daily` /
//! webhook / fs-event) persisted to `~/.origin/schedule.toml`.
//!
//! Spec parsing and next-fire computation come from [`origin_schedule`]. The
//! daemon reads the same file to actually fire triggers; this CLI surface is
//! the management front-end (add / list / remove).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::cli_def::ScheduleSub;

/// One persisted trigger row.
///
/// The schema MUST stay byte-compatible with the daemon's reader
/// (`origin-daemon/src/scheduler.rs`), which reads the SAME
/// `~/.origin/schedule.toml`. The CLI does a full read-modify-write through this
/// struct on every `add`/`rm`, so any daemon-only field absent here would be
/// SILENTLY DROPPED on the next edit. `profile` + `env` mirror the daemon's
/// per-trigger fields exactly (names, types, and serde defaults), and the
/// `skip_serializing_if` attributes keep a trigger with neither field
/// byte-identical to the pre-profile schema on round-trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TriggerEntry {
    id: String,
    spec: String,
    prompt: String,
    /// Name of a reusable `[profiles.<name>]` variable set to apply when this
    /// trigger fires. Mirrors the daemon's `TriggerEntry::profile`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile: Option<String>,
    /// Inline per-trigger variables, layered OVER the named `profile`. Mirrors
    /// the daemon's `TriggerEntry::env`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
}

/// On-disk schedule file.
///
/// Mirrors the daemon's `ScheduleFile`, including the top-level `[profiles]`
/// table, so a CLI read-modify-write preserves the reusable variable sets the
/// user declared for the daemon.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ScheduleFile {
    #[serde(default)]
    triggers: Vec<TriggerEntry>,
    /// Reusable, named variable sets referenced by `trigger.profile`. Mirrors
    /// the daemon's `ScheduleFile::profiles`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    profiles: BTreeMap<String, BTreeMap<String, String>>,
}

/// Dispatch a `schedule` subcommand.
///
/// # Errors
/// Returns on filesystem / TOML failure or on an invalid schedule spec.
pub fn run(sub: ScheduleSub) -> Result<()> {
    match sub {
        ScheduleSub::Add {
            id,
            spec,
            prompt,
            profile,
            env,
        } => add(id, spec, prompt, profile, &env),
        ScheduleSub::Ls => list(),
        ScheduleSub::Rm { id } => remove(&id),
    }
}

/// Parse repeated `--env KEY=VALUE` specs into a map. Each spec must contain a
/// `=`; the key (left of the first `=`) must be non-empty. The value may be
/// empty or contain further `=` characters.
fn parse_env_specs(specs: &[String]) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for spec in specs {
        let (key, value) = spec
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid --env `{spec}` (expected KEY=VALUE)"))?;
        if key.is_empty() {
            anyhow::bail!("invalid --env `{spec}` (empty key)");
        }
        out.insert(key.to_string(), value.to_string());
    }
    Ok(out)
}

fn store_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let dir = home.join(".origin");
    std::fs::create_dir_all(&dir).map_err(|e| anyhow::anyhow!("creating {}: {e}", dir.display()))?;
    Ok(dir.join("schedule.toml"))
}

fn load() -> Result<ScheduleFile> {
    load_from(&store_path()?)
}

/// Parse the schedule file at `path` (missing ⇒ default/empty). Split out from
/// [`load`] so the round-trip is testable against a temp file.
fn load_from(path: &std::path::Path) -> Result<ScheduleFile> {
    match std::fs::read_to_string(path) {
        Ok(s) => toml::from_str(&s).map_err(|e| anyhow::anyhow!("parsing schedule.toml: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ScheduleFile::default()),
        Err(e) => Err(anyhow::anyhow!("reading schedule.toml: {e}")),
    }
}

/// Serialize `f` to TOML and write it to `path`. Used by the read-modify-write
/// `add`/`remove` cores against the resolved store path (or a temp file in tests).
fn save_to(path: &std::path::Path, f: &ScheduleFile) -> Result<()> {
    let body = toml::to_string_pretty(f).map_err(|e| anyhow::anyhow!("serializing schedule.toml: {e}"))?;
    std::fs::write(path, body).map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
    Ok(())
}

fn add(
    id: String,
    spec: String,
    prompt: String,
    profile: Option<String>,
    env_specs: &[String],
) -> Result<()> {
    let path = store_path()?;
    let added_id = add_in(&path, id, spec, prompt, profile, env_specs)?;
    println!("added trigger `{added_id}`");
    Ok(())
}

/// Read-modify-write `add` against an explicit `path`, returning the added id.
///
/// Persists FIRST and returns the id only once the write lands, so a failed save
/// never wrongly reports success (the caller prints only on `Ok`). Split out from
/// [`add`] so the full read-modify-write — and thus the daemon-field preservation
/// — is testable against a temp file.
fn add_in(
    path: &std::path::Path,
    id: String,
    spec: String,
    prompt: String,
    profile: Option<String>,
    env_specs: &[String],
) -> Result<String> {
    // Validate the spec up front so bad triggers never reach the daemon.
    origin_schedule::parse_schedule(&spec)
        .map_err(|e| anyhow::anyhow!("invalid schedule spec {spec:?}: {e}"))?;
    let env = parse_env_specs(env_specs)?;
    let mut f = load_from(path)?;
    if f.triggers.iter().any(|t| t.id == id) {
        anyhow::bail!("a trigger with id `{id}` already exists");
    }
    let added_id = id.clone();
    f.triggers.push(TriggerEntry {
        id,
        spec,
        prompt,
        profile,
        env,
    });
    save_to(path, &f)?;
    Ok(added_id)
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
    let path = store_path()?;
    if remove_in(&path, id)? {
        println!("removed trigger `{id}`");
    } else {
        println!("no such trigger: `{id}`");
    }
    Ok(())
}

/// Read-modify-write `remove` against an explicit `path`. Returns `true` if a
/// trigger was removed (and the file rewritten), `false` if no such id existed
/// (file untouched). Split out from [`remove`] so the round-trip is testable.
fn remove_in(path: &std::path::Path, id: &str) -> Result<bool> {
    let mut f = load_from(path)?;
    let before = f.triggers.len();
    f.triggers.retain(|t| t.id != id);
    if f.triggers.len() == before {
        Ok(false)
    } else {
        save_to(path, &f)?;
        Ok(true)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{add_in, load_from, parse_env_specs, remove_in};

    /// Regression for findings #7/#9: the CLI's read-modify-write must NOT drop
    /// the daemon-only fields. A `schedule.toml` carrying a `profile` + inline
    /// `[trigger.env]` + a top-level `[profiles]` table must survive an
    /// `add()` + `remove()` round-trip with every daemon field intact.
    #[test]
    fn add_then_remove_preserves_daemon_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schedule.toml");

        // A pre-existing trigger the user authored for the daemon, plus a
        // shared `[profiles]` table — none of which the CLI knows how to author
        // but all of which it must preserve.
        let seed = "\
[[triggers]]
id = \"nightly\"
spec = \"@daily 09:30\"
prompt = \"summarize {{repo}}\"
profile = \"shared\"

[triggers.env]
extra = \"inline-value\"

[profiles.shared]
repo = \"github.com/acme/widget\"
oncall = \"@dana\"
";
        std::fs::write(&path, seed).unwrap();

        // CLI `add` a fresh trigger (with its OWN profile + env), then `remove`
        // it — the classic read-modify-write that previously nuked the data.
        add_in(
            &path,
            "adhoc".to_string(),
            "@every 5m".to_string(),
            "do {{thing}}".to_string(),
            Some("shared".to_string()),
            &["thing=cleanup".to_string()],
        )
        .unwrap();
        assert!(remove_in(&path, "adhoc").unwrap());

        // The seeded trigger + its profile/env + the [profiles] table all survive.
        let f = load_from(&path).unwrap();
        assert_eq!(f.triggers.len(), 1, "adhoc removed, nightly retained");
        let nightly = &f.triggers[0];
        assert_eq!(nightly.id, "nightly");
        assert_eq!(nightly.profile.as_deref(), Some("shared"));
        assert_eq!(nightly.env.get("extra").map(String::as_str), Some("inline-value"));
        let shared = f.profiles.get("shared").expect("[profiles.shared] preserved");
        assert_eq!(
            shared.get("repo").map(String::as_str),
            Some("github.com/acme/widget")
        );
        assert_eq!(shared.get("oncall").map(String::as_str), Some("@dana"));
    }

    /// The `--profile` / `--env` flags are actually authored into the new entry.
    #[test]
    fn add_writes_profile_and_env_flags() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schedule.toml");
        add_in(
            &path,
            "t1".to_string(),
            "@every 1m".to_string(),
            "hi {{a}} {{b}}".to_string(),
            Some("prof".to_string()),
            &["a=1".to_string(), "b=two".to_string()],
        )
        .unwrap();
        let f = load_from(&path).unwrap();
        let t = &f.triggers[0];
        assert_eq!(t.profile.as_deref(), Some("prof"));
        assert_eq!(t.env.get("a").map(String::as_str), Some("1"));
        assert_eq!(t.env.get("b").map(String::as_str), Some("two"));
    }

    /// A trigger with neither flag round-trips byte-identically to the legacy
    /// schema: no `profile`/`env` keys are emitted (`skip_serializing_if`).
    #[test]
    fn add_without_flags_omits_optional_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schedule.toml");
        add_in(
            &path,
            "plain".to_string(),
            "@every 1m".to_string(),
            "go".to_string(),
            None,
            &[],
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("profile"), "no profile key when unset: {body}");
        assert!(!body.contains("env"), "no env table when empty: {body}");
        assert!(!body.contains("profiles"), "no profiles table when empty: {body}");
    }

    #[test]
    fn parse_env_specs_validates() {
        let ok = parse_env_specs(&["k=v".to_string(), "empty=".to_string(), "x=a=b".to_string()]).unwrap();
        assert_eq!(ok.get("k").map(String::as_str), Some("v"));
        assert_eq!(ok.get("empty").map(String::as_str), Some(""));
        assert_eq!(ok.get("x").map(String::as_str), Some("a=b"));
        assert!(parse_env_specs(&["no-equals".to_string()]).is_err());
        assert!(parse_env_specs(&["=novalue".to_string()]).is_err());
    }
}
