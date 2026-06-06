// SPDX-License-Identifier: Apache-2.0
//! Pure-logic scheduling and triggers over millisecond unix timestamps.
//!
//! `origin`'s baseline has no first-class way to fire an agent on a clock the
//! way claude-code's `/schedule` + `/loop`, cline's cron, kilocode's Triggers,
//! and opencode's cron do. This crate supplies the *time arithmetic* for that
//! feature: it parses human schedule specs, computes the next fire time, and
//! manages a small armed-trigger queue.
//!
//! Everything is deterministic math over `u64` milliseconds — no real timers,
//! no threads, no I/O, no clock reads. The daemon owns the wall clock and the
//! tokio timers; this crate only answers "given `now`, when next?" and "which
//! ids are due?". Civil time (year/month/day/hour/minute/day-of-week) is
//! decomposed from the unix epoch with a self-contained UTC algorithm, so the
//! crate is std-only and trivially testable.
//!
//! ```
//! use origin_schedule::{parse_schedule, Schedule};
//!
//! let s = parse_schedule("@every 5m").unwrap();
//! assert_eq!(s, Schedule::Interval { ms: 300_000 });
//! assert_eq!(s.next_after(0), Some(300_000));
//! ```

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Number of milliseconds in one minute.
pub const MS_PER_MINUTE: u64 = 60_000;
/// Number of milliseconds in one hour.
pub const MS_PER_HOUR: u64 = 60 * MS_PER_MINUTE;
/// Number of milliseconds in one day.
pub const MS_PER_DAY: u64 = 24 * MS_PER_HOUR;

/// Hard cap on how far `next_after` will scan a cron schedule (~366 days of
/// minutes). A schedule that never matches within a year returns `None` rather
/// than looping unboundedly.
const CRON_SCAN_MINUTES: u64 = 366 * 24 * 60;

/// One field of a 5-field cron expression.
///
/// Only the subset `origin` needs is modelled: a wildcard or an explicit set of
/// allowed values (a single integer is just a one-element [`Field::Only`]).
/// Ranges and steps are intentionally unsupported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Field {
    /// Matches any value (the cron `*`).
    Any,
    /// Matches when the value is in this set (a single int or comma list).
    Only(Vec<u32>),
}

impl Field {
    /// Returns `true` when `value` satisfies this field.
    #[must_use]
    pub fn matches(&self, value: u32) -> bool {
        match self {
            Self::Any => true,
            Self::Only(set) => set.contains(&value),
        }
    }
}

/// A parsed schedule expression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Schedule {
    /// Fire every `ms` milliseconds (`@every <N><s|m|h|d>`).
    Interval {
        /// Interval length in milliseconds; always non-zero after parsing.
        ms: u64,
    },
    /// Fire once per UTC day at the given minute of day (`@daily HH:MM`).
    DailyAt {
        /// Minutes since UTC midnight, in `0..1440`.
        minute_of_day: u32,
    },
    /// Fire when wall-clock UTC matches all five cron fields.
    Cron {
        /// Minute field, `0..60`.
        min: Field,
        /// Hour field, `0..24`.
        hour: Field,
        /// Day-of-month field, `1..32`.
        dom: Field,
        /// Month field, `1..13`.
        mon: Field,
        /// Day-of-week field, `0..7` (0 and 7 both mean Sunday).
        dow: Field,
    },
}

/// Error produced while parsing a schedule spec.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScheduleError {
    /// The spec was malformed; the payload describes why.
    #[error("invalid schedule spec: {0}")]
    Bad(String),
}

/// Parse a schedule spec into a [`Schedule`].
///
/// Accepted forms:
/// - `@every <N><s|m|h|d>` — a positive interval, e.g. `@every 5m`.
/// - `@daily HH:MM` — once per UTC day at the given time.
/// - `min hour dom mon dow` — a 5-field cron subset where each field is `*`, a
///   single integer, or a comma-separated list (no ranges or steps).
///
/// # Errors
///
/// Returns [`ScheduleError::Bad`] when the spec is empty, has an unknown form,
/// has the wrong field count, contains a non-numeric or out-of-range value, or
/// specifies a zero interval.
pub fn parse_schedule(s: &str) -> Result<Schedule, ScheduleError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(ScheduleError::Bad("empty spec".to_string()));
    }
    if let Some(rest) = trimmed.strip_prefix("@every") {
        return parse_interval(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix("@daily") {
        return parse_daily(rest.trim());
    }
    if trimmed.starts_with('@') {
        return Err(ScheduleError::Bad(format!("unknown directive: {trimmed}")));
    }
    parse_cron(trimmed)
}

fn parse_interval(body: &str) -> Result<Schedule, ScheduleError> {
    if body.len() < 2 {
        return Err(ScheduleError::Bad(format!("bad interval: {body:?}")));
    }
    let (num, unit) = body.split_at(body.len() - 1);
    let n: u64 = num
        .trim()
        .parse()
        .map_err(|_| ScheduleError::Bad(format!("bad interval count: {num:?}")))?;
    if n == 0 {
        return Err(ScheduleError::Bad("interval must be > 0".to_string()));
    }
    let per = match unit {
        "s" => 1_000,
        "m" => MS_PER_MINUTE,
        "h" => MS_PER_HOUR,
        "d" => MS_PER_DAY,
        other => {
            return Err(ScheduleError::Bad(format!(
                "unknown interval unit: {other:?} (use s|m|h|d)"
            )))
        }
    };
    let ms = n
        .checked_mul(per)
        .ok_or_else(|| ScheduleError::Bad("interval overflows u64 milliseconds".to_string()))?;
    Ok(Schedule::Interval { ms })
}

fn parse_daily(body: &str) -> Result<Schedule, ScheduleError> {
    let (hh, mm) = body
        .split_once(':')
        .ok_or_else(|| ScheduleError::Bad(format!("@daily needs HH:MM, got {body:?}")))?;
    let hour: u32 = hh
        .trim()
        .parse()
        .map_err(|_| ScheduleError::Bad(format!("bad hour: {hh:?}")))?;
    let minute: u32 = mm
        .trim()
        .parse()
        .map_err(|_| ScheduleError::Bad(format!("bad minute: {mm:?}")))?;
    if hour > 23 {
        return Err(ScheduleError::Bad(format!("hour out of range: {hour}")));
    }
    if minute > 59 {
        return Err(ScheduleError::Bad(format!("minute out of range: {minute}")));
    }
    Ok(Schedule::DailyAt {
        minute_of_day: hour * 60 + minute,
    })
}

// `min`/`mon` and `dom`/`dow` are the canonical cron field names; keeping them
// is far clearer than contrived distinct spellings.
#[allow(clippy::similar_names)]
fn parse_cron(body: &str) -> Result<Schedule, ScheduleError> {
    let parts: Vec<&str> = body.split_whitespace().collect();
    if parts.len() != 5 {
        return Err(ScheduleError::Bad(format!(
            "cron needs 5 fields (min hour dom mon dow), got {}",
            parts.len()
        )));
    }
    let min = parse_field(parts[0], 0, 59)?;
    let hour = parse_field(parts[1], 0, 23)?;
    let dom = parse_field(parts[2], 1, 31)?;
    let mon = parse_field(parts[3], 1, 12)?;
    let dow = parse_field(parts[4], 0, 7)?;
    Ok(Schedule::Cron {
        min,
        hour,
        dom,
        mon,
        dow,
    })
}

fn parse_field(raw: &str, lo: u32, hi: u32) -> Result<Field, ScheduleError> {
    if raw == "*" {
        return Ok(Field::Any);
    }
    let mut set = Vec::new();
    for piece in raw.split(',') {
        let piece = piece.trim();
        let value: u32 = piece
            .parse()
            .map_err(|_| ScheduleError::Bad(format!("not an integer: {piece:?}")))?;
        if value < lo || value > hi {
            return Err(ScheduleError::Bad(format!(
                "value {value} out of range {lo}..={hi}"
            )));
        }
        set.push(value);
    }
    if set.is_empty() {
        return Err(ScheduleError::Bad("empty field".to_string()));
    }
    Ok(Field::Only(set))
}

impl Schedule {
    /// Smallest fire time strictly greater than `now_unix_ms`, or `None` when
    /// the schedule has no future match within its scan window.
    ///
    /// For [`Schedule::Interval`] this snaps `now` up to the next multiple of
    /// the interval (treating the epoch as the phase origin). For
    /// [`Schedule::DailyAt`] it returns the next occurrence of the minute-of-day.
    /// For [`Schedule::Cron`] it scans minute-by-minute up to a ~1 year cap,
    /// matching every field against UTC civil time.
    #[must_use]
    pub fn next_after(&self, now_unix_ms: u64) -> Option<u64> {
        match self {
            Self::Interval { ms } => {
                if *ms == 0 {
                    return None;
                }
                // Next multiple of `ms` strictly after `now`, phase-aligned to
                // the epoch so repeated calls form a stable cadence.
                let next = now_unix_ms / ms * ms + ms;
                Some(next)
            }
            Self::DailyAt { minute_of_day } => {
                let target_ms = u64::from(*minute_of_day) * MS_PER_MINUTE;
                let day_start = now_unix_ms / MS_PER_DAY * MS_PER_DAY;
                let today = day_start + target_ms;
                if today > now_unix_ms {
                    Some(today)
                } else {
                    Some(today + MS_PER_DAY)
                }
            }
            Self::Cron {
                min,
                hour,
                dom,
                mon,
                dow,
            } => {
                // Start at the next whole minute strictly after `now`.
                let start_minute = now_unix_ms / MS_PER_MINUTE + 1;
                for offset in 0..CRON_SCAN_MINUTES {
                    let minute_index = start_minute + offset;
                    let civil = Civil::from_unix_ms(minute_index * MS_PER_MINUTE);
                    if min.matches(civil.minute)
                        && hour.matches(civil.hour)
                        && mon.matches(civil.month)
                        && Self::matches_day(dom, dow, &civil)
                    {
                        return Some(minute_index * MS_PER_MINUTE);
                    }
                }
                None
            }
        }
    }

    /// Cron day matching: when both day-of-month and day-of-week are restricted,
    /// vixie-cron fires if *either* matches; otherwise the restricted field (or
    /// both wildcards) governs.
    #[allow(clippy::similar_names)] // `dom`/`dow` are canonical cron field names
    fn matches_day(dom: &Field, dow: &Field, civil: &Civil) -> bool {
        let dom_restricted = matches!(dom, Field::Only(_));
        let dow_restricted = matches!(dow, Field::Only(_));
        let dom_ok = dom.matches(civil.day);
        // Cron Sunday is 0, but 7 is also accepted; normalise both directions.
        let dow_ok = dow.matches(civil.dow) || (civil.dow == 0 && dow.matches(7));
        if dom_restricted && dow_restricted {
            dom_ok || dow_ok
        } else {
            dom_ok && dow_ok
        }
    }
}

/// UTC civil-time decomposition of a unix-millisecond instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Civil {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    /// Day of week, `0..7` with 0 = Sunday.
    dow: u32,
}

impl Civil {
    /// Decompose unix milliseconds into UTC civil fields.
    ///
    /// Uses Howard Hinnant's `civil_from_days` algorithm for the date part,
    /// which is exact for the proleptic Gregorian calendar and needs no leap
    /// tables. All arithmetic is integer-only.
    fn from_unix_ms(ms: u64) -> Self {
        let total_minutes = ms / MS_PER_MINUTE;
        let minute = u32::try_from(total_minutes % 60).unwrap_or(0);
        let total_hours = total_minutes / 60;
        let hour = u32::try_from(total_hours % 24).unwrap_or(0);
        // Unix-day index since 1970-01-01. `u64::MAX` ms is ~5.8e11 days, well
        // within `i64`, so the conversion never wraps in practice.
        let days = i64::try_from(total_hours / 24).unwrap_or(i64::MAX);

        // 1970-01-01 was a Thursday (=4 with Sunday=0).
        let weekday = u32::try_from((days % 7 + 4 + 7) % 7).unwrap_or(0);

        // civil_from_days (Howard Hinnant): shift epoch to 0000-03-01.
        let shifted = days + 719_468;
        let era = if shifted >= 0 { shifted } else { shifted - 146_096 } / 146_097;
        let day_of_era = shifted - era * 146_097; // [0, 146096]
        let year_of_era =
            (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
        let civil_year = year_of_era + era * 400;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
        let month_index = (5 * day_of_year + 2) / 153; // [0, 11], March-based
        let day_of_month = day_of_year - (153 * month_index + 2) / 5 + 1; // [1, 31]
        let calendar_month = if month_index < 10 {
            month_index + 3
        } else {
            month_index - 9
        }; // [1, 12]
        let year = if calendar_month <= 2 {
            civil_year + 1
        } else {
            civil_year
        };

        Self {
            year,
            month: u32::try_from(calendar_month).unwrap_or(1),
            day: u32::try_from(day_of_month).unwrap_or(1),
            hour,
            minute,
            dow: weekday,
        }
    }
}

/// Kind of trigger that drives an agent run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerKind {
    /// Time-based trigger governed by a [`Schedule`].
    Schedule(Schedule),
    /// HTTP webhook trigger on the given path; has no time schedule.
    Webhook {
        /// Path the daemon listens on (e.g. `/hooks/deploy`).
        path: String,
    },
    /// Filesystem-event trigger on the given glob; has no time schedule.
    FsEvent {
        /// Glob whose matched paths fire the trigger (e.g. `src/**/*.rs`).
        glob: String,
    },
}

impl TriggerKind {
    /// Next fire time for this kind, or `None` for non-time triggers
    /// ([`TriggerKind::Webhook`] / [`TriggerKind::FsEvent`]).
    #[must_use]
    pub fn next_after(&self, now_unix_ms: u64) -> Option<u64> {
        match self {
            Self::Schedule(s) => s.next_after(now_unix_ms),
            Self::Webhook { .. } | Self::FsEvent { .. } => None,
        }
    }
}

/// A configured trigger: an identity, what fires it, and what to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trigger {
    /// Stable unique identifier.
    pub id: String,
    /// What causes this trigger to fire.
    pub kind: TriggerKind,
    /// Prompt template handed to the agent when fired.
    pub prompt_template: String,
    /// Extra environment variables to inject into the run.
    pub env: Vec<(String, String)>,
}

impl Trigger {
    /// Construct a trigger with no environment overrides.
    #[must_use]
    pub fn new(id: impl Into<String>, kind: TriggerKind, prompt_template: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind,
            prompt_template: prompt_template.into(),
            env: Vec::new(),
        }
    }

    /// Next fire time for this trigger, delegating to its [`TriggerKind`].
    #[must_use]
    pub fn next_after(&self, now_unix_ms: u64) -> Option<u64> {
        self.kind.next_after(now_unix_ms)
    }
}

/// A min-time priority queue of armed trigger fire times.
///
/// The daemon arms a trigger id with its next computed fire time; on each tick
/// it pops everything that has come due. Storage is a flat `Vec` kept sorted by
/// fire time descending so the soonest entry sits at the tail for cheap pops.
#[derive(Debug, Clone, Default)]
pub struct Queue {
    // Invariant: sorted by `at_ms` descending, so the earliest entry is last.
    armed: Vec<(u64, String)>,
}

impl Queue {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of armed entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.armed.len()
    }

    /// Whether the queue has no armed entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.armed.is_empty()
    }

    /// Earliest armed fire time, if any.
    #[must_use]
    pub fn peek_next_at(&self) -> Option<u64> {
        self.armed.last().map(|(at, _)| *at)
    }

    /// Arm `id` to fire at `at_ms`. Multiple arms of the same id are kept as
    /// distinct entries; callers that want replace-semantics should
    /// [`Queue::cancel`] first.
    pub fn arm(&mut self, id: String, at_ms: u64) {
        // Insert keeping the descending-by-time invariant.
        let pos = self
            .armed
            .binary_search_by(|(at, _)| at_ms.cmp(at))
            .unwrap_or_else(|p| p);
        self.armed.insert(pos, (at_ms, id));
    }

    /// Remove all armed entries for `id`, returning how many were removed.
    pub fn cancel(&mut self, id: &str) -> usize {
        let before = self.armed.len();
        self.armed.retain(|(_, armed_id)| armed_id != id);
        before - self.armed.len()
    }

    /// Pop and return the ids of every entry armed at or before `now_ms`.
    ///
    /// Returned ids are ordered earliest-fire-first; entries that are still in
    /// the future remain armed.
    pub fn due(&mut self, now_ms: u64) -> Vec<String> {
        let mut fired = Vec::new();
        while let Some((at, _)) = self.armed.last() {
            if *at <= now_ms {
                // Safe: we just checked `last()` is `Some`.
                if let Some((_, id)) = self.armed.pop() {
                    fired.push(id);
                }
            } else {
                break;
            }
        }
        fired
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn parse_every_minutes() {
        let s = parse_schedule("@every 5m").unwrap();
        assert_eq!(s, Schedule::Interval { ms: 300_000 });
    }

    #[test]
    fn parse_every_all_units() {
        assert_eq!(
            parse_schedule("@every 30s").unwrap(),
            Schedule::Interval { ms: 30_000 }
        );
        assert_eq!(
            parse_schedule("@every 2h").unwrap(),
            Schedule::Interval { ms: 2 * MS_PER_HOUR }
        );
        assert_eq!(
            parse_schedule("@every 1d").unwrap(),
            Schedule::Interval { ms: MS_PER_DAY }
        );
    }

    #[test]
    fn interval_next_after_is_phase_aligned() {
        let s = Schedule::Interval { ms: 300_000 };
        assert_eq!(s.next_after(0), Some(300_000));
        // Mid-window snaps up to the next boundary.
        assert_eq!(s.next_after(300_001), Some(600_000));
        // Exactly on a boundary advances to the next one (strictly future).
        assert_eq!(s.next_after(300_000), Some(600_000));
    }

    #[test]
    fn daily_fires_at_minute_of_day() {
        let s = parse_schedule("@daily 09:30").unwrap();
        assert_eq!(
            s,
            Schedule::DailyAt {
                minute_of_day: 9 * 60 + 30
            }
        );
        // At UTC midnight, the next 09:30 is later the same day.
        let expected = (9 * 60 + 30) * MS_PER_MINUTE;
        assert_eq!(s.next_after(0), Some(expected));
        // One ms after today's 09:30, it rolls to tomorrow.
        assert_eq!(s.next_after(expected), Some(expected + MS_PER_DAY));
    }

    #[test]
    fn cron_parses_and_finds_next_nine_am() {
        let s = parse_schedule("0 9 * * *").unwrap();
        assert_eq!(
            s,
            Schedule::Cron {
                min: Field::Only(vec![0]),
                hour: Field::Only(vec![9]),
                dom: Field::Any,
                mon: Field::Any,
                dow: Field::Any,
            }
        );
        // From epoch (1970-01-01 00:00 UTC), next 09:00 is 9 hours in.
        assert_eq!(s.next_after(0), Some(9 * MS_PER_HOUR));
        // The Civil decomposition lands exactly on 09:00.
        let fire = s.next_after(0).unwrap();
        let civil = Civil::from_unix_ms(fire);
        assert_eq!((civil.hour, civil.minute), (9, 0));
    }

    #[test]
    fn cron_comma_list_field() {
        let s = parse_schedule("0 0,12 * * *").unwrap();
        // Next midnight or noon after 06:00 on day 0 is 12:00 the same day.
        let six_am = 6 * MS_PER_HOUR;
        assert_eq!(s.next_after(six_am), Some(12 * MS_PER_HOUR));
        // After noon, the next is the following midnight.
        assert_eq!(s.next_after(12 * MS_PER_HOUR), Some(MS_PER_DAY));
    }

    #[test]
    fn cron_day_of_week_matches_known_date() {
        // 1970-01-01 was a Thursday (dow=4). Fire at 00:00 on Thursdays.
        let s = parse_schedule("0 0 * * 4").unwrap();
        // Strictly after epoch midnight, the next Thursday midnight is 7 days on.
        assert_eq!(s.next_after(1), Some(7 * MS_PER_DAY));
    }

    #[test]
    fn cron_specific_date_decomposition() {
        // 2000-01-01 00:00:00 UTC = 946_684_800_000 ms; it was a Saturday (dow=6).
        let civil = Civil::from_unix_ms(946_684_800_000);
        assert_eq!((civil.year, civil.month, civil.day), (2000, 1, 1));
        assert_eq!(civil.dow, 6);
        // 2024-02-29 (leap day) 12:19 UTC.
        let leap = Civil::from_unix_ms(1_709_209_140_000);
        assert_eq!((leap.year, leap.month, leap.day), (2024, 2, 29));
        assert_eq!((leap.hour, leap.minute), (12, 19));
    }

    #[test]
    fn webhook_and_fsevent_have_no_schedule() {
        let wh = TriggerKind::Webhook {
            path: "/hooks/x".to_string(),
        };
        let fs = TriggerKind::FsEvent {
            glob: "src/**/*.rs".to_string(),
        };
        assert_eq!(wh.next_after(1_000), None);
        assert_eq!(fs.next_after(1_000), None);
    }

    #[test]
    fn trigger_delegates_next_after() {
        let t = Trigger::new(
            "nightly",
            TriggerKind::Schedule(Schedule::DailyAt { minute_of_day: 0 }),
            "run nightly checks",
        );
        assert_eq!(t.next_after(1), Some(MS_PER_DAY));
        assert!(t.env.is_empty());
    }

    #[test]
    fn queue_due_pops_due_keeps_future() {
        let mut q = Queue::new();
        q.arm("a".to_string(), 100);
        q.arm("c".to_string(), 300);
        q.arm("b".to_string(), 200);
        assert_eq!(q.len(), 3);
        assert_eq!(q.peek_next_at(), Some(100));
        // At t=200, a and b are due (earliest first); c remains.
        let fired = q.due(200);
        assert_eq!(fired, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(q.len(), 1);
        assert_eq!(q.peek_next_at(), Some(300));
        // Nothing new is due yet.
        assert!(q.due(250).is_empty());
        // Finally c fires.
        assert_eq!(q.due(300), vec!["c".to_string()]);
        assert!(q.is_empty());
    }

    #[test]
    fn queue_cancel_removes_entries() {
        let mut q = Queue::new();
        q.arm("a".to_string(), 100);
        q.arm("a".to_string(), 150);
        q.arm("b".to_string(), 200);
        assert_eq!(q.cancel("a"), 2);
        assert_eq!(q.len(), 1);
        assert_eq!(q.cancel("missing"), 0);
    }

    #[test]
    fn bad_specs_error() {
        assert!(parse_schedule("").is_err());
        assert!(parse_schedule("@every").is_err());
        assert!(parse_schedule("@every 0m").is_err());
        assert!(parse_schedule("@every 5x").is_err());
        assert!(parse_schedule("@daily 25:00").is_err());
        assert!(parse_schedule("@daily 9").is_err());
        assert!(parse_schedule("@weekly").is_err());
        assert!(parse_schedule("0 9 * *").is_err()); // 4 fields
        assert!(parse_schedule("60 9 * * *").is_err()); // minute out of range
        assert!(parse_schedule("x 9 * * *").is_err()); // non-numeric
    }

    #[test]
    fn field_matches() {
        assert!(Field::Any.matches(42));
        assert!(Field::Only(vec![1, 2, 3]).matches(2));
        assert!(!Field::Only(vec![1, 2, 3]).matches(4));
    }
}
