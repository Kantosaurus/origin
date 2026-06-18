# origin-router

> Model routing strategies (architect/editor split, phase-aware, scored, quota fallback) over fed-in health/latency

## Purpose

`origin-router` adds pluggable model routing so a session can split work across
models: an architect/editor split, phase-aware planning-vs-fast routing, a
health-scored picker, and a quota-fallback chain. It is pure logic — no network.
Latency and error signals are fed in via `Router::record_result` and folded into
an exponential moving average, so the crate is trivially testable and free of I/O
concerns.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `ModelRef` | struct | A `provider` + `model` pair; `key()` is `provider/model`. |
| `Phase` | enum | `Plan`, `Edit`, `Execute`, `Default` — what a turn is doing. |
| `Strategy` | enum | `Fixed`, `ArchitectEditor`, `PhaseAware`, `Scored`, `QuotaFallback`. |
| `Health` | struct | Per-model `ema_latency_ms`, `ema_error_rate`, `exhausted`; `score()`. |
| `Router` | struct | Applies a `Strategy`, tracks `Health` per model. |
| `Router::{new, try_new, choose, record_result, scored_order, mark_exhausted, clear_exhausted}` | fn | Construct, route, and feed signals. |
| `rank_by_latency` | fn | Pure helper: rank candidates by measured latency, lowest first. |
| `EMA_ALPHA` | const | `0.3` smoothing factor for the latency / error EMA. |
| `RouterError` | enum | `EmptyChain` (a `QuotaFallback` with no chain). |

## Key types

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Strategy {
    Fixed(ModelRef),
    ArchitectEditor { architect: ModelRef, editor: ModelRef },
    PhaseAware { plan: ModelRef, fast: ModelRef },
    Scored,
    QuotaFallback { chain: Vec<ModelRef> },
}

pub struct Router { /* strategy, health: HashMap<String, Health> */ }

impl Router {
    pub fn choose(&self, phase: Phase, candidates: &[ModelRef]) -> Option<ModelRef>;
    pub fn record_result(&mut self, m: &ModelRef, latency_ms: u64, ok: bool);
    pub fn scored_order(&self, candidates: &[ModelRef]) -> Vec<ModelRef>;
}
```

## How it works

**Selection.** `Router::choose(phase, candidates)` dispatches on the strategy:

- `Fixed` always returns its model (ignores candidates).
- `ArchitectEditor` returns the architect for `Phase::Plan`, the editor otherwise.
- `PhaseAware` returns `plan` for `Phase::Plan`, `fast` otherwise.
- `QuotaFallback` returns the first non-exhausted model in its chain.
- `Scored` ranks `candidates` by `Health::score`, skipping exhausted models, and
  returns the best — or `None` when candidates are empty / all exhausted.

**Health signals.** `record_result(m, latency_ms, ok)` folds an observation into
that model's EMAs. The *first* observation seeds the average directly (so a single
sample is not diluted toward zero); later samples blend with `EMA_ALPHA`. The
routing score is `(1 - error_rate) / max(latency, 1.0)` — both low error and low
latency raise it.

**Exhaustion.** `mark_exhausted` / `clear_exhausted` flip a per-model
quota/rate-limit flag that `QuotaFallback` and `Scored` skip; `is_exhausted`
queries it. Clearing a never-seen model is a no-op, not a panic.

**Validation.** `try_new` rejects a `QuotaFallback` with an empty chain
(`RouterError::EmptyChain`); `new` constructs unconditionally.

**Helpers.** `scored_order` returns the full best-first ranking (not just the top
pick), independent of the configured strategy. `rank_by_latency(samples)` is the
pure ranking behind `origin providers recommend`: it builds a scored router from
`(ModelRef, latency_ms)` samples and returns the lowest-latency-first order.

## Dependencies & features

Only `serde` (derive) and `thiserror`; `serde_json` is a dev-dependency for the
serde round-trip test. No async, no network, no other origin crates. No cargo
features.

## Used by

`Grep "origin-router"` over `crates/*/Cargo.toml`:

```
crates/origin-cli/Cargo.toml
crates/origin-daemon/Cargo.toml
crates/origin-router/Cargo.toml
```

## Testing

All coverage is in-file (`#[cfg(test)] mod tests`): per-phase selection for each
strategy, quota fallback skip/recover, scored preference for low-latency/low-error,
exhausted-skipping, EMA seeding and blending math, `scored_order` ranking,
`rank_by_latency` ordering, `try_new` empty-chain rejection, and a `Strategy`
serde round-trip.

## See also

- [Providers subsystem](../subsystems/providers.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
