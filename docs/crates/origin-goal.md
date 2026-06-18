# origin-goal

> Goal driver with persistent completion conditions and an inline self-tag protocol.

## Purpose

`origin-goal` carries the types and pure-function transitions for a persistent
*goal driver*: the agent is given a completion condition and keeps iterating
until that condition is met, the budget/iteration cap is hit, a verifier
rejects too many times, or the user clears it. The main model self-reports
progress inline via `<goal-status>` tags, which this crate parses tolerantly.
The async driver (with providers and tokio) lives in `origin-daemon`; this crate
stays dependency-free so the state machine and tag parser can be unit-tested in
isolation.

## Public API surface

| Item | Kind | Description |
|------|------|-------------|
| `GoalState` | struct | Live goal: condition, status, iter/budget counters, last tag. |
| `GoalStatus` | enum | `Active` / `Verifying` / `Met` / `Cleared`. |
| `ClearReason` | enum | Why a goal ended (user, max-iter, budget, verifier, blocked, …). |
| `TagOutcome` | enum | Parsed `<goal-status>`: `Met` / `InProgress` / `Blocked` / `Missing`. |
| `parse_tag` | fn | Parse the rightmost well-formed `<goal-status>` tag from model text. |
| `Verifier` (in `verifier`) | trait | Async one-shot verification → `(Verdict, in_tok, out_tok)`. |
| `Verdict` | enum | `Met` / `NotMet { reason }`. |
| `parse_goal_args` / `GoalArgs` | fn / struct | Parse inline goal-activation flags. |
| `GoalSnapshot` and `*Wire` types | struct/enum | Serde-safe persistence shapes. |
| `DEFAULT_MAX_ITER` / `DEFAULT_TOKEN_BUDGET` | const | 20 iterations / 200k tokens. |

Module map: `flags`, `state`, `tag`, `verifier`, `wire`.

## Key types

```rust
pub enum TagOutcome { Met, InProgress { what_remains: String }, Blocked { why: String }, Missing }

pub struct GoalState {
    pub condition: String,
    pub status: GoalStatus,
    pub iter: u32, pub max_iter: u32,
    pub tokens_spent: u64, pub token_budget: u64,
    pub last_status_tag: Option<TagOutcome>,
    pub consecutive_rejections: u32,         // capped at MAX_CONSECUTIVE_VERIFIER_REJECTIONS (3)
}

pub enum ClearReason {
    UserSlash, UserClearAll, MaxIter, BudgetExhausted,
    VerifierRejected(String), Met { reason: String },
    VerifierUnavailable, Blocked { why: String },
}

#[async_trait]
pub trait Verifier: Send + Sync {
    async fn verify(&self, condition: &str, last_turn: &str)
        -> Result<(Verdict, u64, u64), VerifierError>;
}
```

## How it works

Each turn the main model emits a `<goal-status state="...">reason</goal-status>`
tag. `parse_tag` is deliberately tolerant — case-insensitive `state=`,
whitespace in attributes, missing reason defaults to empty, **last tag wins** —
so a forgetful model never accidentally ends the loop (unknown/missing →
`TagOutcome::Missing`, which keeps iterating).

```
self-report tag ─► TagOutcome
   Met        ─► run Verifier ─► Met  ─► GoalStatus::Met / Cleared{Met}
                               └ NotMet ─► consecutive_rejections++ ─► cap(3) ─► Cleared{VerifierRejected}
   InProgress ─► iter++  (reset rejections)  ─► continue, or cap on max_iter / budget
   Blocked    ─► Cleared{Blocked}  (stop, hand back to the human)
   Missing    ─► continue (no termination)
```

A `Met` tag is *not* trusted on its own — it triggers an independent `Verifier`
pass (the daemon supplies an Anthropic-Haiku implementation), and the verifier's
token spend is charged against the goal's budget. Three consecutive rejections
end the goal.

## Dependencies & features

- Runtime deps: `serde` (derive), `thiserror`, `async-trait` (for `Verifier`).
  No tokio, no providers — those live in the daemon's `goal_driver`.
- Dev-deps: `tokio` (macros/rt) for async verifier tests. `#![forbid(unsafe_code)]`.

## Used by

`Grep "origin-goal" glob "crates/*/Cargo.toml"` →

- `crates/origin-cli/Cargo.toml`
- `crates/origin-daemon/Cargo.toml`
- `crates/origin-goal/Cargo.toml` (self)
- `crates/origin-resume-token/Cargo.toml`

The daemon's `goal_driver`, `goal_checkpoint`, and `goal_clear_all` modules host
the live driver; `origin-resume-token` persists goal snapshots across restarts.

## Testing

`tests/` directory: `tag_parser.rs` (tolerant parsing edge cases),
`state_machine.rs` (transition coverage), `verifier_mock.rs` (an inline
`MockVerifier`), and `flag_parser.rs` (inline goal-activation flags).

## See also

- [Agent & sessions subsystem](../subsystems/agent-and-sessions.md)
- [Swarm & orchestration subsystem](../subsystems/swarm-and-orchestration.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
