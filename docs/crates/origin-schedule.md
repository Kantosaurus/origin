# origin-schedule

> Pure-logic scheduling, cron/interval/daily spec parsing, and trigger queue over millisecond timestamps.

## Purpose

`origin-schedule` supplies the *time arithmetic* for firing an agent on a clock
(the `/schedule` + `/loop` style feature). It parses human schedule specs,
computes the next fire time after a given instant, and matches cron fields
against UTC civil time. Everything is deterministic integer math over `u64`
milliseconds — no real timers, threads, I/O, or clock reads — so the daemon
owns the wall clock and tokio timers while this crate only answers "given
`now`, when next?".

## Public API surface

| Item | Kind | Description |
|------|------|-------------|
| `parse_schedule` | fn | Parse a spec string into a `Schedule`. |
| `Schedule` | enum | `Interval { ms }` / `DailyAt { minute_of_day }` / `Cron { .. }`. |
| `Schedule::next_after` | method | Smallest fire time strictly greater than `now_unix_ms`. |
| `Field` | enum | One cron field: `Any` (`*`) or `Only(Vec<u32>)`. |
| `Field::matches` | method | Whether a value satisfies the field. |
| `ScheduleError` | enum | `Bad(String)` parse failure. |
| `MS_PER_MINUTE` / `MS_PER_HOUR` / `MS_PER_DAY` | const | Time unit constants. |

## Key types

```rust
pub enum Schedule {
    Interval { ms: u64 },                                  // @every 5m
    DailyAt  { minute_of_day: u32 },                       // @daily HH:MM
    Cron { min: Field, hour: Field, dom: Field, mon: Field, dow: Field },
}

impl Schedule {
    #[must_use]
    pub fn next_after(&self, now_unix_ms: u64) -> Option<u64>;
}

pub fn parse_schedule(s: &str) -> Result<Schedule, ScheduleError>;
```

## How it works

Three accepted spec forms:

- `@every <N><s|m|h|d>` → `Interval`; `next_after` snaps `now` up to the next
  multiple of the interval, phase-aligned to the Unix epoch so repeated calls
  form a stable cadence.
- `@daily HH:MM` → `DailyAt`; returns the next occurrence of that minute-of-day
  (today if still future, otherwise tomorrow).
- `min hour dom mon dow` → `Cron`; each field is `*`, a single integer, or a
  comma list (ranges/steps intentionally unsupported).

```
parse_schedule("0 9 * * 1,5")
   └─► Cron { min:Only[0], hour:Only[9], dom:Any, mon:Any, dow:Only[1,5] }

next_after(now): scan minute-by-minute up to ~366 days; for each candidate,
   decompose to UTC Civil time (Howard Hinnant civil_from_days, integer-only)
   and match every field. Vixie-cron day rule: when BOTH dom and dow are
   restricted, fire if EITHER matches; otherwise both must match.
```

A `CRON_SCAN_MINUTES` cap (~1 year) means a never-matching cron returns `None`
rather than looping unboundedly.

## Dependencies & features

- Runtime deps: `serde` (derive), `thiserror`. `std`-only — civil time is
  decomposed with a self-contained UTC algorithm, no `chrono`/`time`.
- `#![forbid(unsafe_code)]`. No Cargo features.

## Used by

`Grep "origin-schedule" glob "crates/*/Cargo.toml"` →

- `crates/origin-cli/Cargo.toml`
- `crates/origin-daemon/Cargo.toml`
- `crates/origin-schedule/Cargo.toml` (self)

The daemon's `scheduler` module arms tokio timers from `next_after` and runs
the trigger queue; the CLI parses and displays schedules.

## Testing

In-file `#[cfg(test)] mod tests`: interval parsing for all units, phase-aligned
`next_after`, daily fire timing, cron 9am/comma-list/day-of-week against known
dates, exact `Civil` decomposition (incl. a 2024 leap day), and a battery of
malformed-spec rejections.

## See also

- [Swarm & orchestration subsystem](../subsystems/swarm-and-orchestration.md)
- [Agent & sessions subsystem](../subsystems/agent-and-sessions.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
