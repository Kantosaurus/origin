# origin-cost

> Per-turn and cumulative USD cost + token accounting with prompt-cache economy awareness

## Purpose

`origin-cost` provides per-turn and cumulative USD cost + token accounting for
origin sessions, plus prompt-cache economy awareness (a "your cache went cold"
signal). It is pure arithmetic — no I/O, no async — so it is trivially testable
and free of platform concerns. It ships a builtin per-model price table, a
running `CostMeter`, and an `Insights` report.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `TokenUsage` | struct | Per-turn token counts: `input`, `output`, `cache_read`, `cache_write`. |
| `Cost` | struct | USD broken out by category; `total()`, `microdollars()`, `plus()`. |
| `ModelPrice` | struct | USD per 1M tokens for each category. |
| `cost_of` | fn | Compute a `Cost` from a `ModelPrice` + `TokenUsage`. |
| `price_for` | fn | Longest-prefix price lookup (strips provider prefix), `None` if unknown. |
| `CostMeter` | struct | Running accumulator; `record`, `cumulative`, `insights`. |
| `TurnCost` / `ModelLine` / `Insights` | struct | Per-turn, per-model, and session report shapes. |
| `is_cache_cold` | fn | Decide whether a turn started against a cold prompt cache. |
| `PROMPT_CACHE_TTL_MS` | const | ~5 min ephemeral prompt-cache lifetime. |
| `fmt_usd` | fn | Compact USD formatting (`$0.0023`, `$1.42`, `$128`). |

## Key types

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage { pub input: u64, pub output: u64, pub cache_read: u64, pub cache_write: u64 }

pub struct CostMeter { /* cumulative usage/cost, turns, last_turn_at_ms, cold_cache_turns */ }

impl CostMeter {
    pub fn record(&mut self, model: &str, usage: TokenUsage, now_ms: u64) -> TurnCost;
    pub fn cumulative(&self) -> &Cost;
    pub fn insights(&self) -> Insights;
}

pub fn price_for(model: &str) -> Option<ModelPrice>;
pub fn cost_of(price: &ModelPrice, usage: &TokenUsage) -> Cost;
```

## How it works

**Pricing.** `price_for` normalises a model id (lowercase, strip a `provider/` or
`provider:` prefix) and does a longest-prefix match against the static `PRICES`
table — so `claude-3-5-haiku` wins over the broad `claude-` entry. Unknown models
return `None`, letting the UI show tokens without a misleading dollar figure.
`ModelPrice::flat` applies Anthropic-style cache multipliers (read 0.1×, write
1.25×) when a provider publishes no separate cache rates; `cached` sets all four
explicitly. `cost_of` is per-million arithmetic across the four categories.

**Accounting.** `CostMeter::record(model, usage, now_ms)` prices the turn, folds
it into cumulative usage/cost, retains the per-turn `TurnCost`, and returns it.
`insights()` builds a per-model breakdown sorted by descending cost, with totals
and a `cold_cache_turns` count.

**Cache warmth.** A turn is *cold* when more than `PROMPT_CACHE_TTL_MS` elapsed
since the previous turn (the ephemeral cache likely expired, re-paying the
cache-write premium). `CostMeter` tracks this internally (`TurnCost.cache_warm`);
the standalone `is_cache_cold(prev_turn_ms, now_ms, cache_read_tokens,
had_prior_warm)` adds a second arm: zero cache reads inside the TTL after a prior
warm turn is also cold (the provider signalled the entry is gone). The first turn
of a session is always warm.

**Display.** `microdollars()` gives integer-safe USD×1e6 for sub-cent turns;
`fmt_usd` buckets a float into `$0` / `$0.0004` / `$1.42` / `$128`.

## Dependencies & features

Only `serde` (derive). No async, no network, no other origin crates. No cargo
features. (`TokenUsage` mirrors `origin_provider::Usage`'s shape so the daemon can
convert without a lossy intermediate.)

## Used by

`Grep "origin-cost"` over `crates/*/Cargo.toml`:

```
crates/origin-cli/Cargo.toml
crates/origin-cost/Cargo.toml
crates/origin-daemon/Cargo.toml
```

## Testing

All coverage is in-file (`#[cfg(test)] mod tests`): longest-prefix price lookup +
provider-prefix stripping, per-million cost math, microdollar rounding,
`CostMeter` accumulation and per-model breakdown, cold-cache detection after the
TTL, every arm of `is_cache_cold`, unpriced-model handling, cache-hit-rate
fraction, and `fmt_usd` buckets.

## See also

- [Providers subsystem](../subsystems/providers.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
