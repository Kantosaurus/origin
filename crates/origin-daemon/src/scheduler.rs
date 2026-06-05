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
    /// Name of a reusable `[profiles.<name>]` variable set to apply when this
    /// trigger fires (its `{{key}}` entries become template variables). Absent
    /// ⇒ no profile vars, byte-identical to the pre-profile schema.
    #[serde(default)]
    profile: Option<String>,
    /// Inline per-trigger variables, layered OVER the named `profile` (so a
    /// trigger can reuse a shared profile yet override one value). Empty ⇒ none.
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
}

/// On-disk schedule file.
#[derive(Debug, Default, Deserialize)]
struct ScheduleFile {
    #[serde(default)]
    triggers: Vec<TriggerEntry>,
    /// Reusable, named variable sets referenced by `trigger.profile`. Each is a
    /// `{{key}} -> value` map merged into a fired trigger's template variables,
    /// so common context (repo URL, base branch, on-call handle, …) is declared
    /// once and shared across many webhook/cron triggers. Built-in vars
    /// (`{{date}}`, `{{trigger_id}}`, …) always win on a name clash.
    #[serde(default)]
    profiles: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
}

/// A trigger that came due on the current tick, paired with the prompt to fire
/// and its resolved profile/inline template variables.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DueTrigger {
    id: String,
    spec: String,
    prompt: String,
    /// Resolved `(name, value)` template vars from the trigger's `profile` +
    /// inline `env` (inline overrides profile). Empty for triggers with neither.
    vars: Vec<(String, String)>,
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
            // Read the wall clock once at the call site so the expander stays
            // pure; `fire_vars` builds the fixed template variables from the
            // trigger plus this timestamp, then the trigger's resolved profile /
            // inline vars are appended (built-ins listed FIRST ⇒ they win on a
            // name clash, since `expand_template` takes the first match), then
            // `expand_template` substitutes.
            let mut vars: Vec<(&str, String)> = Vec::new();
            for (k, v) in fire_vars(&due, now_ms()) {
                vars.push((k, v));
            }
            for (k, v) in &due.vars {
                vars.push((k.as_str(), v.clone()));
            }
            let prompt = expand_template(&due.prompt, &vars);
            if let Err(e) = dispatch_prompt(&sock_path, &model, session_id, &prompt).await {
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
                    spec: t.spec.clone(),
                    prompt: t.prompt.clone(),
                    vars: resolve_trigger_vars(&file.profiles, t.profile.as_deref(), &t.env),
                });
            }
        }
    }
    due
}

/// Open a fresh client connection to the daemon's own IPC socket and submit
/// `prompt` as a `Prompt`.
///
/// Drains the response stream to completion. Best-effort: any transport error is
/// returned to the caller for logging without crashing the tick loop. Shared
/// with the ambient / webhook / self-dev paths so a fired trigger / ambient task
/// / self-edit runs through the real agent loop.
///
/// Behaviourally identical to (and a thin wrapper over)
/// [`dispatch_prompt_with_usage`]; the accumulated turn usage is discarded so
/// the ambient/webhook/scheduler callers keep their `Result<(), String>` shape.
///
/// # Errors
/// Returns the rendered transport / provider / loop error string when the
/// connection cannot be opened or the daemon emits an error frame for the turn.
pub async fn dispatch_prompt(
    sock_path: &str,
    model: &str,
    session_id: String,
    prompt: &str,
) -> Result<(), String> {
    dispatch_prompt_with_usage(sock_path, model, session_id, prompt)
        .await
        .map(|_tokens| ())
}

/// Same connect/send/drain as [`dispatch_prompt`] but returns the turn's total
/// token usage (sum of `input_tokens + output_tokens` across every
/// [`StreamEvent::Usage`](crate::protocol::StreamEvent) in the reply stream).
///
/// Returns `0` when the daemon emitted no `Usage` event, letting the overnight
/// loop fall back to its estimate via `observe_task_tokens(None)`. Any transport
/// or provider error is surfaced as `Err` exactly as `dispatch_prompt` does.
pub(crate) async fn dispatch_prompt_with_usage(
    sock_path: &str,
    model: &str,
    session_id: String,
    prompt: &str,
) -> Result<u64, String> {
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
    // terminal `PromptReply`. Each decoded `StreamEvent::Usage` adds its
    // input+output tokens to the running total returned to the caller.
    let mut tokens = 0u64;
    loop {
        // Connection closed ⇒ turn finished (or daemon shut down).
        let Ok((kind, frame)) = conn.read_frame().await else {
            break;
        };
        if matches!(kind, FrameKind::ErrorFrame) {
            return Err(String::from_utf8_lossy(&frame).into_owned());
        }
        if let Ok(event) = serde_json::from_slice::<crate::protocol::StreamEvent>(&frame) {
            tokens = tokens.saturating_add(accumulate_usage(std::slice::from_ref(&event)));
            continue;
        }
        break;
    }
    Ok(tokens)
}

/// Sum `input_tokens + output_tokens` across every
/// [`StreamEvent::Usage`](crate::protocol::StreamEvent) in `events`.
///
/// Pure and dependency-free so the per-turn accounting is unit-testable without
/// a live daemon: non-`Usage` events contribute nothing and an empty / usage-free
/// slice yields `0`.
fn accumulate_usage(events: &[crate::protocol::StreamEvent]) -> u64 {
    events
        .iter()
        .filter_map(|event| match event {
            crate::protocol::StreamEvent::Usage {
                input_tokens,
                output_tokens,
                ..
            } => Some(u64::from(*input_tokens) + u64::from(*output_tokens)),
            _ => None,
        })
        .sum()
}

/// Current wall-clock time in milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Substitute a fixed set of `{{name}}` template variables in `prompt`.
///
/// Pure and clock-free: callers pass the already-resolved `vars` (built by
/// [`fire_vars`] from a trigger and a timestamp). Each `{{name}}` whose `name`
/// (trimmed of surrounding spaces) appears in `vars` is replaced by its value;
/// any other `{{...}}` run is copied through verbatim, so a prompt with no
/// recognised placeholders is returned byte-identical. This keeps trigger
/// prompts free to mention literal braces without erroring.
fn expand_template(prompt: &str, vars: &[(&str, String)]) -> String {
    // Walk the bytes, copying through everything except a recognised
    // `{{name}}`. We index on byte positions; placeholder names are ASCII in
    // practice, and any non-match is emitted unchanged so UTF-8 is preserved.
    let bytes = prompt.as_bytes();
    let mut out = String::with_capacity(prompt.len());
    let mut i = 0;
    while i < bytes.len() {
        if let Some(end) = placeholder_end(prompt, i) {
            // `i+2..end-2` is the inner name (strip the `{{` and `}}`). A known
            // var contributes its value; an unknown one copies the whole
            // `{{...}}` run verbatim, leaving it for the agent to see as-is.
            let name = prompt[i + 2..end - 2].trim();
            let replacement = vars
                .iter()
                .find(|(key, _)| *key == name)
                .map_or(&prompt[i..end], |(_, value)| value.as_str());
            out.push_str(replacement);
            i = end;
        } else {
            // Not the start of a placeholder; emit this char and advance past
            // its full UTF-8 width so multi-byte chars are never split.
            let ch_len = char_len_at(bytes, i);
            out.push_str(&prompt[i..i + ch_len]);
            i += ch_len;
        }
    }
    out
}

/// If a well-formed `{{...}}` placeholder starts at byte `start`, return the
/// byte index just past its closing `}}`; otherwise `None`.
///
/// A placeholder is `{{`, a run of non-`{`/`}` bytes (possibly empty), then
/// `}}`. An unterminated `{{` or one containing a stray brace is not a
/// placeholder, so it falls through to verbatim copying.
fn placeholder_end(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.get(start) != Some(&b'{') || bytes.get(start + 1) != Some(&b'{') {
        return None;
    }
    let mut j = start + 2;
    while let Some(&b) = bytes.get(j) {
        if b == b'}' {
            // A closing `}}` (need the second `}`) ends the placeholder.
            return (bytes.get(j + 1) == Some(&b'}')).then_some(j + 2);
        }
        if b == b'{' {
            // A nested `{` breaks the placeholder; treat as literal.
            return None;
        }
        j += 1;
    }
    None
}

/// Byte width of the UTF-8 character starting at `bytes[i]` (assumes `i` is a
/// char boundary, which the [`expand_template`] walk guarantees).
fn char_len_at(bytes: &[u8], i: usize) -> usize {
    match bytes.get(i) {
        Some(b) if *b < 0x80 => 1,
        Some(b) if *b >= 0xF0 => 4,
        Some(b) if *b >= 0xE0 => 3,
        Some(_) => 2,
        None => 1,
    }
}

/// Build the fixed template variables for a fired trigger at `now_unix_ms`.
///
/// Returns `{{date}}`, `{{time}}`, `{{datetime}}`, `{{weekday}}` (all UTC, from
/// the passed-in timestamp so the assembler stays clock-free and testable) plus
/// `{{trigger_id}}` and `{{trigger_spec}}` from the trigger itself.
fn fire_vars(due: &DueTrigger, now_unix_ms: u64) -> Vec<(&'static str, String)> {
    let (date, time) = civil_strings(now_unix_ms);
    let datetime = format!("{date} {time}");
    vec![
        ("date", date),
        ("time", time),
        ("datetime", datetime),
        ("weekday", weekday_name(now_unix_ms).to_string()),
        ("trigger_id", due.id.clone()),
        ("trigger_spec", due.spec.clone()),
    ]
}

/// Resolve a trigger's reusable-profile + inline variables into ordered
/// `(name, value)` pairs.
///
/// The named `profile` (if it exists in `profiles`) is laid down first, then the
/// trigger's inline `env` is layered OVER it, so a trigger can reuse a shared
/// profile yet override individual values. A missing/`None` profile contributes
/// nothing. Pure and dependency-free so it is unit-testable without a runtime.
fn resolve_trigger_vars(
    profiles: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    profile: Option<&str>,
    inline: &std::collections::BTreeMap<String, String>,
) -> Vec<(String, String)> {
    let mut merged: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    if let Some(name) = profile {
        if let Some(p) = profiles.get(name) {
            for (k, v) in p {
                merged.insert(k.clone(), v.clone());
            }
        }
    }
    // Inline env overrides the profile.
    for (k, v) in inline {
        merged.insert(k.clone(), v.clone());
    }
    merged.into_iter().collect()
}

/// Decompose a unix-millisecond instant into UTC `("YYYY-MM-DD", "HH:MM:SS")`.
///
/// Integer-only, std-only, no new dependency: the date part uses Howard
/// Hinnant's `civil_from_days` algorithm (same approach `origin-schedule`'s
/// internal `Civil` uses), which is exact for the proleptic Gregorian calendar.
// `day_of_era`/`day_of_year`/`year_of_era`/`civil_year` are the canonical names
// from the published algorithm; renaming for distinctness would obscure it.
#[allow(clippy::similar_names)]
fn civil_strings(ms: u64) -> (String, String) {
    let total_secs = ms / 1_000;
    let second = total_secs % 60;
    let total_minutes = total_secs / 60;
    let minute = total_minutes % 60;
    let total_hours = total_minutes / 60;
    let hour = total_hours % 24;
    let days = i64::try_from(total_hours / 24).unwrap_or(i64::MAX);

    // civil_from_days: shift the epoch to 0000-03-01 to make leap handling regular.
    let shifted = days + 719_468;
    let era = if shifted >= 0 { shifted } else { shifted - 146_096 } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let civil_year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day_of_month = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = if month <= 2 { civil_year + 1 } else { civil_year };

    let date = format!("{year:04}-{month:02}-{day_of_month:02}");
    let time = format!("{hour:02}:{minute:02}:{second:02}");
    (date, time)
}

/// UTC weekday name for a unix-millisecond instant (1970-01-01 was a Thursday).
fn weekday_name(ms: u64) -> &'static str {
    const NAMES: [&str; 7] = [
        "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
    ];
    let days = i64::try_from(ms / (24 * 60 * 60 * 1_000)).unwrap_or(i64::MAX);
    // Sunday=0; epoch Thursday=4. Euclidean rem keeps the index in 0..7.
    let idx = usize::try_from((days + 4).rem_euclid(7)).unwrap_or(0);
    NAMES.get(idx).copied().unwrap_or("Sunday")
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::{
        accumulate_usage, civil_strings, due_triggers, expand_template, fire_vars,
        resolve_trigger_vars, weekday_name, DueTrigger, ScheduleFile,
    };
    use crate::protocol::StreamEvent;

    /// A `StreamEvent::Usage` with the given input/output tokens (cache fields 0).
    const fn usage(input_tokens: u32, output_tokens: u32) -> StreamEvent {
        StreamEvent::Usage {
            input_tokens,
            output_tokens,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        }
    }

    #[test]
    fn accumulate_usage_sums_input_and_output_across_events() {
        let events = [usage(100, 25), usage(200, 50)];
        // (100+25) + (200+50) summed across both Usage events.
        assert_eq!(accumulate_usage(&events), 375);
    }

    #[test]
    fn accumulate_usage_is_zero_without_usage_events() {
        // No Usage events at all (and the empty slice) yield 0 so the overnight
        // loop falls back to its estimate via observe_task_tokens(None).
        assert_eq!(accumulate_usage(&[]), 0);
        let non_usage = [
            StreamEvent::TextDelta { text: "hi".to_string() },
            StreamEvent::TurnEnd,
        ];
        assert_eq!(accumulate_usage(&non_usage), 0);
    }

    #[test]
    fn accumulate_usage_ignores_non_usage_events() {
        // Interleaved non-Usage events contribute nothing; only Usage counts.
        let events = [
            StreamEvent::TextDelta { text: "x".to_string() },
            usage(10, 5),
            StreamEvent::TurnEnd,
            usage(1, 1),
        ];
        assert_eq!(accumulate_usage(&events), 17);
    }

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

    #[test]
    fn expand_substitutes_known_vars() {
        let vars = vec![
            ("date", "2026-05-31".to_string()),
            ("trigger_id", "nightly".to_string()),
        ];
        let out = expand_template("On {{date}} run {{trigger_id}}", &vars);
        assert_eq!(out, "On 2026-05-31 run nightly");
    }

    #[test]
    fn expand_leaves_unknown_placeholders_verbatim() {
        let vars = vec![("date", "2026-05-31".to_string())];
        // Unknown `{{x}}` is copied through untouched; known one still expands.
        let out = expand_template("{{unknown}} and {{date}} and {{nope}}", &vars);
        assert_eq!(out, "{{unknown}} and 2026-05-31 and {{nope}}");
    }

    #[test]
    fn expand_handles_repeated_and_adjacent_placeholders() {
        let vars = vec![
            ("a", "X".to_string()),
            ("b", "Y".to_string()),
        ];
        // Repeated and back-to-back placeholders with no separator.
        let out = expand_template("{{a}}{{b}}{{a}} {{a}}", &vars);
        assert_eq!(out, "XYX X");
    }

    #[test]
    fn expand_trims_inner_whitespace_in_names() {
        let vars = vec![("date", "D".to_string())];
        assert_eq!(expand_template("{{ date }}", &vars), "D");
    }

    #[test]
    fn expand_no_placeholder_prompt_is_unchanged() {
        let vars = vec![("date", "2026-05-31".to_string())];
        let prompt = "plain prompt with no braces and emoji 🚀 and a brace { left open";
        assert_eq!(expand_template(prompt, &vars), prompt);
    }

    #[test]
    fn expand_unterminated_and_single_brace_are_literal() {
        let vars = vec![("date", "D".to_string())];
        // Unterminated `{{date` and a lone `{` are not placeholders.
        assert_eq!(expand_template("{{date", &vars), "{{date");
        assert_eq!(expand_template("a { b } c", &vars), "a { b } c");
        // A single-brace `{date}` is not a placeholder either.
        assert_eq!(expand_template("{date}", &vars), "{date}");
    }

    #[test]
    fn civil_strings_match_known_instants() {
        // 1970-01-01 00:00:00 UTC.
        assert_eq!(civil_strings(0), ("1970-01-01".to_string(), "00:00:00".to_string()));
        // 2024-02-29 (leap day) 12:19:30 UTC = 1_709_209_170_000 ms.
        assert_eq!(
            civil_strings(1_709_209_170_000),
            ("2024-02-29".to_string(), "12:19:30".to_string())
        );
    }

    #[test]
    fn weekday_names_match_known_dates() {
        // Epoch is a Thursday; one day later (86_400_000 ms) is Friday.
        assert_eq!(weekday_name(0), "Thursday");
        assert_eq!(weekday_name(86_400_000), "Friday");
        // 2000-01-01 was a Saturday (946_684_800_000 ms).
        assert_eq!(weekday_name(946_684_800_000), "Saturday");
    }

    #[test]
    fn resolve_trigger_vars_merges_profile_then_inline_override() {
        use std::collections::BTreeMap;
        let mut profiles: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut prod: BTreeMap<String, String> = BTreeMap::new();
        prod.insert("repo".to_string(), "acme/app".to_string());
        prod.insert("branch".to_string(), "main".to_string());
        profiles.insert("prod".to_string(), prod);

        // No profile / no inline ⇒ empty.
        assert!(resolve_trigger_vars(&profiles, None, &BTreeMap::new()).is_empty());
        // Unknown profile contributes nothing.
        assert!(resolve_trigger_vars(&profiles, Some("nope"), &BTreeMap::new()).is_empty());

        // Profile alone, then inline overriding one key + adding another.
        let mut inline: BTreeMap<String, String> = BTreeMap::new();
        inline.insert("branch".to_string(), "release".to_string());
        inline.insert("urgent".to_string(), "true".to_string());
        let vars = resolve_trigger_vars(&profiles, Some("prod"), &inline);
        // BTreeMap ⇒ deterministic ascending key order.
        assert_eq!(
            vars,
            vec![
                ("branch".to_string(), "release".to_string()), // inline overrode profile
                ("repo".to_string(), "acme/app".to_string()),
                ("urgent".to_string(), "true".to_string()),
            ]
        );
    }

    #[test]
    fn profile_vars_expand_but_never_shadow_builtins() {
        use std::collections::BTreeMap;
        // A profile var named `date` must NOT override the built-in {{date}}.
        let mut profile_clash: BTreeMap<String, String> = BTreeMap::new();
        profile_clash.insert("date".to_string(), "SHADOW".to_string());
        let due = DueTrigger {
            id: "t".to_string(),
            spec: "@daily 00:00".to_string(),
            prompt: String::new(),
            vars: profile_clash.into_iter().collect::<Vec<_>>(),
        };
        // Built-ins first ⇒ they win on a name clash (expand_template = first match).
        let mut vars: Vec<(&str, String)> = Vec::new();
        for (k, v) in fire_vars(&due, 0) {
            vars.push((k, v));
        }
        for (k, v) in &due.vars {
            vars.push((k.as_str(), v.clone()));
        }
        assert_eq!(expand_template("{{date}}", &vars), "1970-01-01");

        // A non-builtin profile var DOES expand.
        let due2 = DueTrigger {
            id: "t".to_string(),
            spec: "@daily 00:00".to_string(),
            prompt: String::new(),
            vars: vec![("repo".to_string(), "acme/app".to_string())],
        };
        let mut vars2: Vec<(&str, String)> = Vec::new();
        for (k, v) in fire_vars(&due2, 0) {
            vars2.push((k, v));
        }
        for (k, v) in &due2.vars {
            vars2.push((k.as_str(), v.clone()));
        }
        assert_eq!(expand_template("repo={{repo}}", &vars2), "repo=acme/app");
    }

    #[test]
    fn fire_vars_builds_all_six_and_expands_end_to_end() {
        let due = DueTrigger {
            id: "nightly".to_string(),
            spec: "@daily 03:00".to_string(),
            prompt: String::new(),
            vars: Vec::new(),
        };
        // Epoch: 1970-01-01 00:00:00, a Thursday.
        let vars = fire_vars(&due, 0);
        let keys: Vec<&str> = vars.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            keys,
            vec!["date", "time", "datetime", "weekday", "trigger_id", "trigger_spec"]
        );
        let prompt =
            "[{{datetime}}] ({{weekday}}) id={{trigger_id}} spec={{trigger_spec}} on {{date}} at {{time}}";
        assert_eq!(
            expand_template(prompt, &vars),
            "[1970-01-01 00:00:00] (Thursday) id=nightly spec=@daily 03:00 on 1970-01-01 at 00:00:00"
        );
    }
}
