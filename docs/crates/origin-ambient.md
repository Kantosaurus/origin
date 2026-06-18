# origin-ambient

> Resource-aware always-on + overnight autonomous mode policy under an adaptive token budget.

## Purpose

`origin-ambient` is the pure **policy** layer for proactive background work —
running tests, small refactors, doc touch-ups, and "memory gardening" while the
user is idle or asleep. It decides *when* ambient work may run under an adaptive
token budget that always reserves headroom for the interactive user, picks the
*next* task round-robin, names a PR-gated *branch*, drives an *overnight* plan
through a wall-clock window, and assembles a *morning report*. It performs no
execution, I/O, async, or clock reads — the daemon owns the loop and supplies
`now_ms`.

## Public API surface

| Item | Kind | Description |
|------|------|-------------|
| `IdleTracker` | struct | Lock-free (`AtomicU64`) tracker of user idle time for ambient gating. |
| `BudgetPolicy` | struct | Adaptive daily token budget that protects a user reserve. |
| `AmbientTask` | enum | `Tests` / `Refactor` / `Docs` / `MemoryGarden`. |
| `next_task` | fn | Round-robin pick that never immediately repeats. |
| `branch_name` | fn | PR-gated branch slug, e.g. `origin/ambient/tests-20234`. |
| `should_schedule` | fn | Whether `now_min` is inside an overnight window (handles midnight wrap). |
| `OvernightPlan` | struct | Ordered task list + wall-clock ceiling. |
| `OvernightDriver` | struct | Pure driver that walks a plan and accumulates outcomes. |
| `MorningReport` | struct | Markdown-rendered summary of an overnight session. |
| `DEFAULT_MIN_IDLE_MS` | const | 5-minute idle gap before ambient work may run. |

## Key types

```rust
pub struct BudgetPolicy { pub total_daily_tokens: u64, pub reserve_for_user: u64 }
impl BudgetPolicy {
    pub const fn available(&self, spent_today: u64) -> u64;        // saturating
    pub const fn may_run(&self, spent_today: u64, est_cost: u64) -> bool;
}

pub enum AmbientTask { Tests, Refactor, Docs, MemoryGarden }
pub fn next_task(recent: &[AmbientTask]) -> AmbientTask;           // no immediate repeat

pub struct OvernightDriver { /* plan, start_ms, cursor, ran, tokens, prs */ }
impl OvernightDriver {
    pub fn next_due(&self, now_ms: u64) -> Option<AmbientTask>;    // peek, no advance
    pub fn record(&mut self, task: AmbientTask, tokens: u64, pr: Option<String>);
    pub fn is_finished(&self, now_ms: u64) -> bool;
    pub fn into_report(self, day_unix: u64) -> MorningReport;
}
```

## How it works

The budget guarantee is `available = (total - reserve) - spent`, saturating to
zero; `may_run` only returns `true` when an estimate fits entirely inside that
non-reserved headroom, so ambient work can never starve a user session. The
`OvernightDriver` is a cursor over an `OvernightPlan`:

```
new(plan, start_ms)
  │
  ├─ next_due(now) ──► Some(task)  while  now-start < max_wall_ms  AND  tasks remain
  │                    None        otherwise   (window closed or plan consumed)
  │
  ├─ record(task, tokens, pr?)  ─► advance cursor, accumulate
  │
  └─ into_report(day_unix) ─► MorningReport { ran, tokens_spent, prs_opened, worktrees }
```

`next_due` is a *peek* (it never advances); the loop must call `record` after
running each task. `should_schedule` treats `window_start > window_end` as a
wrap past midnight (e.g. 22:00–06:00) and equal bounds as always-on.

## Dependencies & features

- Runtime deps: `serde` (derive) only — every type is serde round-trippable so
  reports persist as `latest.json`. `MorningReport.worktrees` uses
  `#[serde(default)]` for backward compatibility with pre-worktree reports.
- Dev-deps: `serde_json`. `#![forbid(unsafe_code)]`. No Cargo features.

## Used by

`Grep "origin-ambient" glob "crates/*/Cargo.toml"` →

- `crates/origin-ambient/Cargo.toml` (self)
- `crates/origin-cli/Cargo.toml`
- `crates/origin-daemon/Cargo.toml`

The daemon (`ambient`/`overnight` modules) owns the wall clock and tokio
timers and calls into this policy; the CLI surfaces the morning report.

## Testing

Extensive in-file `#[cfg(test)] mod tests`: idle-clock growth/reset and
monotonic `note_activity`, budget never dipping into the reserve, reserve
clamped to total, round-robin avoiding repeats, branch-name format, morning
report Markdown (with/without worktrees, byte-identical empty case),
serde round-trips including legacy JSON, and full overnight-driver loops.

## See also

- [Swarm & orchestration subsystem](../subsystems/swarm-and-orchestration.md)
- [Agent & sessions subsystem](../subsystems/agent-and-sessions.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
