// SPDX-License-Identifier: Apache-2.0
//! Per-turn and cumulative cost + token accounting for `origin` sessions.
//!
//! `origin`'s baseline tracks cache-hit rate and RSS internally but surfaces no
//! user-facing dollar cost. This crate closes that gap (aider `/tokens`,
//! claude-code `/usage` + `/insights`, kilocode microdollar tracking) and adds
//! prompt-cache economy awareness (jcode's "cache went cold" warning).
//!
//! The crate is pure arithmetic — no I/O, no async — so it is trivially testable
//! and free of platform concerns.
//!
//! ```
//! use origin_cost::{CostMeter, TokenUsage};
//!
//! let mut meter = CostMeter::new();
//! let turn = meter.record("claude-sonnet-4-6", TokenUsage::new(1_000, 500, 0, 2_000), 0);
//! assert!(turn.cost.total() > 0.0);
//! assert!(meter.cumulative().total() > 0.0);
//! ```

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Lifetime of Anthropic's default ephemeral prompt cache (~5 minutes).
///
/// After this gap the next request re-pays the cache-write premium instead of
/// the cheap cache-read. Used to flag "your cache just went cold" (jcode parity).
pub const PROMPT_CACHE_TTL_MS: u64 = 5 * 60 * 1_000;

/// Token counts for a single provider turn.
///
/// Fields mirror [`origin_provider`'s `Usage`] shape so the daemon can convert
/// without a lossy intermediate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Fresh (uncached) input tokens billed at the input rate.
    pub input: u64,
    /// Generated output tokens.
    pub output: u64,
    /// Input tokens served from the prompt cache (billed at the read rate).
    pub cache_read: u64,
    /// Input tokens written into the prompt cache (billed at the write rate).
    pub cache_write: u64,
}

impl TokenUsage {
    /// Construct a usage record.
    #[must_use]
    pub const fn new(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_write,
        }
    }

    /// Total tokens that touched the model this turn (every category).
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    /// Fraction of *input* tokens served from cache, in `[0, 1]`. Returns 0 when
    /// there were no input tokens at all.
    #[must_use]
    pub fn cache_hit_rate(&self) -> f64 {
        let cached = self.cache_read;
        let total_in = self.input + self.cache_read + self.cache_write;
        if total_in == 0 {
            return 0.0;
        }
        as_f64(cached) / as_f64(total_in)
    }

    /// Element-wise sum, for accumulating across turns.
    #[must_use]
    pub const fn plus(self, other: Self) -> Self {
        Self {
            input: self.input + other.input,
            output: self.output + other.output,
            cache_read: self.cache_read + other.cache_read,
            cache_write: self.cache_write + other.cache_write,
        }
    }
}

/// Price for a model, expressed as USD per **one million** tokens of each kind.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelPrice {
    /// USD / 1M fresh input tokens.
    pub input_per_mtok: f64,
    /// USD / 1M output tokens.
    pub output_per_mtok: f64,
    /// USD / 1M cache-read input tokens.
    pub cache_read_per_mtok: f64,
    /// USD / 1M cache-write input tokens.
    pub cache_write_per_mtok: f64,
}

impl ModelPrice {
    const fn flat(input: f64, output: f64) -> Self {
        // Sensible Anthropic-style cache multipliers when a provider does not
        // publish separate cache rates: read = 0.1x input, write = 1.25x input.
        Self {
            input_per_mtok: input,
            output_per_mtok: output,
            cache_read_per_mtok: input * 0.1,
            cache_write_per_mtok: input * 1.25,
        }
    }

    const fn cached(input: f64, output: f64, cache_read: f64, cache_write: f64) -> Self {
        Self {
            input_per_mtok: input,
            output_per_mtok: output,
            cache_read_per_mtok: cache_read,
            cache_write_per_mtok: cache_write,
        }
    }
}

/// USD cost broken out by token category.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    /// Cost of fresh input tokens.
    pub input_usd: f64,
    /// Cost of output tokens.
    pub output_usd: f64,
    /// Cost of cache-read tokens.
    pub cache_read_usd: f64,
    /// Cost of cache-write tokens.
    pub cache_write_usd: f64,
}

impl Cost {
    /// Total USD across all categories.
    #[must_use]
    pub fn total(&self) -> f64 {
        self.input_usd + self.output_usd + self.cache_read_usd + self.cache_write_usd
    }

    /// Total expressed in microdollars (USD × 1e6), rounded — kilocode parity for
    /// integer-safe accounting and display of sub-cent turns.
    #[must_use]
    pub fn microdollars(&self) -> u64 {
        let v = (self.total() * 1_000_000.0).round();
        if v <= 0.0 {
            0
        } else {
            // `v` is finite and non-negative here; the cast is intentional.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                v as u64
            }
        }
    }

    /// Element-wise sum, for accumulating across turns.
    #[must_use]
    pub fn plus(self, other: Self) -> Self {
        Self {
            input_usd: self.input_usd + other.input_usd,
            output_usd: self.output_usd + other.output_usd,
            cache_read_usd: self.cache_read_usd + other.cache_read_usd,
            cache_write_usd: self.cache_write_usd + other.cache_write_usd,
        }
    }
}

/// Compute the cost of `usage` under `price`.
#[must_use]
pub fn cost_of(price: &ModelPrice, usage: &TokenUsage) -> Cost {
    let per = |tokens: u64, rate: f64| as_f64(tokens) / 1_000_000.0 * rate;
    Cost {
        input_usd: per(usage.input, price.input_per_mtok),
        output_usd: per(usage.output, price.output_per_mtok),
        cache_read_usd: per(usage.cache_read, price.cache_read_per_mtok),
        cache_write_usd: per(usage.cache_write, price.cache_write_per_mtok),
    }
}

/// Look up the price for `model` by longest-prefix match against the builtin
/// table. Matching is case-insensitive and ignores a provider prefix such as
/// `anthropic/` or `openai:`.
///
/// Returns `None` for unknown models so callers can show tokens without a
/// (misleading) dollar figure.
#[must_use]
pub fn price_for(model: &str) -> Option<ModelPrice> {
    let needle = normalize(model);
    PRICES
        .iter()
        .filter(|(prefix, _)| needle.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, price)| *price)
}

fn normalize(model: &str) -> String {
    let lower = model.to_ascii_lowercase();
    let tail = lower
        .rsplit_once('/')
        .map_or(lower.as_str(), |(_, t)| t)
        .rsplit_once(':')
        .map_or_else(
            || lower.rsplit_once('/').map_or(lower.as_str(), |(_, t)| t),
            |(_, t)| t,
        );
    tail.to_string()
}

/// Builtin price table (USD / 1M tokens). Longest-prefix wins, so more specific
/// model families can override broader ones. Prices are approximate list rates
/// and intentionally easy to amend; the *mechanism* is the contribution.
static PRICES: &[(&str, ModelPrice)] = &[
    // Anthropic Claude (published cache read/write rates).
    ("claude-opus-4", ModelPrice::cached(15.0, 75.0, 1.5, 18.75)),
    ("claude-sonnet-4", ModelPrice::cached(3.0, 15.0, 0.3, 3.75)),
    ("claude-haiku-4", ModelPrice::cached(0.8, 4.0, 0.08, 1.0)),
    ("claude-3-5-sonnet", ModelPrice::cached(3.0, 15.0, 0.3, 3.75)),
    ("claude-3-5-haiku", ModelPrice::cached(0.8, 4.0, 0.08, 1.0)),
    ("claude-3-opus", ModelPrice::cached(15.0, 75.0, 1.5, 18.75)),
    ("claude-3-haiku", ModelPrice::cached(0.25, 1.25, 0.03, 0.3)),
    ("claude-", ModelPrice::cached(3.0, 15.0, 0.3, 3.75)),
    // OpenAI.
    ("gpt-4o-mini", ModelPrice::flat(0.15, 0.6)),
    ("gpt-4o", ModelPrice::flat(2.5, 10.0)),
    ("gpt-4.1-mini", ModelPrice::flat(0.4, 1.6)),
    ("gpt-4.1", ModelPrice::flat(2.0, 8.0)),
    ("o1-mini", ModelPrice::flat(1.1, 4.4)),
    ("o1", ModelPrice::flat(15.0, 60.0)),
    ("o3-mini", ModelPrice::flat(1.1, 4.4)),
    ("o3", ModelPrice::flat(2.0, 8.0)),
    ("gpt-", ModelPrice::flat(2.5, 10.0)),
    // Google Gemini.
    ("gemini-1.5-pro", ModelPrice::flat(1.25, 5.0)),
    ("gemini-1.5-flash", ModelPrice::flat(0.075, 0.3)),
    ("gemini-2.0-flash", ModelPrice::flat(0.1, 0.4)),
    ("gemini-2.5-pro", ModelPrice::flat(1.25, 10.0)),
    ("gemini-2.5-flash", ModelPrice::flat(0.3, 2.5)),
    ("gemini-", ModelPrice::flat(1.25, 5.0)),
    // Open / aggregator models.
    ("deepseek-chat", ModelPrice::flat(0.27, 1.1)),
    ("deepseek-reasoner", ModelPrice::flat(0.55, 2.19)),
    ("deepseek", ModelPrice::flat(0.27, 1.1)),
    ("grok-", ModelPrice::flat(2.0, 10.0)),
    ("qwen", ModelPrice::flat(0.4, 1.2)),
    ("mistral-large", ModelPrice::flat(2.0, 6.0)),
    ("mistral", ModelPrice::flat(0.2, 0.6)),
    ("llama", ModelPrice::flat(0.2, 0.6)),
];

/// Cost of a single recorded turn, with the model and cache-warmth context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnCost {
    /// Model that served the turn.
    pub model: String,
    /// Token counts for the turn.
    pub usage: TokenUsage,
    /// Computed USD cost. Zero across the board when the model is unpriced.
    pub cost: Cost,
    /// `true` when the model had a published price; `false` means `cost` is a
    /// best-effort zero and the UI should show tokens only.
    pub priced: bool,
    /// `false` when more than [`PROMPT_CACHE_TTL_MS`] elapsed since the previous
    /// turn — the prompt cache had likely expired, so this turn re-paid the
    /// cache-write premium. The UI surfaces this as a gentle warning.
    pub cache_warm: bool,
}

/// Aggregated per-model line in an [`Insights`] report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelLine {
    /// Model identifier.
    pub model: String,
    /// Number of turns served by this model.
    pub turns: u32,
    /// Summed tokens.
    pub usage: TokenUsage,
    /// Summed cost.
    pub cost: Cost,
}

/// A session cost report (claude-code `/insights` parity).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Insights {
    /// Total turns recorded.
    pub turns: u32,
    /// Total tokens across all turns.
    pub usage: TokenUsage,
    /// Total cost across all turns.
    pub cost: Cost,
    /// Per-model breakdown, sorted by descending cost.
    pub per_model: Vec<ModelLine>,
    /// Number of turns that started against a cold prompt cache.
    pub cold_cache_turns: u32,
}

/// Running cost accumulator for a session.
#[derive(Debug, Clone, Default)]
pub struct CostMeter {
    cumulative_usage: TokenUsage,
    cumulative_cost: Cost,
    turns: Vec<TurnCost>,
    last_turn_at_ms: Option<u64>,
    cold_cache_turns: u32,
}

impl CostMeter {
    /// Empty meter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a turn served by `model` with `usage` at wall-clock `now_ms`.
    /// Returns the per-turn breakdown (also retained internally).
    pub fn record(&mut self, model: &str, usage: TokenUsage, now_ms: u64) -> TurnCost {
        let price = price_for(model);
        let cost = price.map_or_else(Cost::default, |p| cost_of(&p, &usage));
        let cache_warm = self
            .last_turn_at_ms
            .is_none_or(|prev| now_ms.saturating_sub(prev) <= PROMPT_CACHE_TTL_MS);
        if !cache_warm {
            self.cold_cache_turns += 1;
        }

        self.cumulative_usage = self.cumulative_usage.plus(usage);
        self.cumulative_cost = self.cumulative_cost.plus(cost);
        self.last_turn_at_ms = Some(now_ms);

        let turn = TurnCost {
            model: model.to_string(),
            usage,
            cost,
            priced: price.is_some(),
            cache_warm,
        };
        self.turns.push(turn.clone());
        turn
    }

    /// Cumulative cost so far.
    #[must_use]
    pub const fn cumulative(&self) -> &Cost {
        &self.cumulative_cost
    }

    /// Cumulative tokens so far.
    #[must_use]
    pub const fn cumulative_usage(&self) -> &TokenUsage {
        &self.cumulative_usage
    }

    /// Number of turns recorded.
    #[must_use]
    pub fn turn_count(&self) -> usize {
        self.turns.len()
    }

    /// Build a per-model insights report.
    #[must_use]
    pub fn insights(&self) -> Insights {
        let mut lines: Vec<ModelLine> = Vec::new();
        for t in &self.turns {
            if let Some(line) = lines.iter_mut().find(|l| l.model == t.model) {
                line.turns += 1;
                line.usage = line.usage.plus(t.usage);
                line.cost = line.cost.plus(t.cost);
            } else {
                lines.push(ModelLine {
                    model: t.model.clone(),
                    turns: 1,
                    usage: t.usage,
                    cost: t.cost,
                });
            }
        }
        lines.sort_by(|a, b| {
            b.cost
                .total()
                .partial_cmp(&a.cost.total())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Insights {
            turns: u32::try_from(self.turns.len()).unwrap_or(u32::MAX),
            usage: self.cumulative_usage,
            cost: self.cumulative_cost,
            per_model: lines,
            cold_cache_turns: self.cold_cache_turns,
        }
    }
}

/// Format a USD amount compactly: `$0.0023`, `$1.42`, `$128`.
#[must_use]
pub fn fmt_usd(usd: f64) -> String {
    if usd <= 0.0 {
        "$0".to_string()
    } else if usd < 0.01 {
        format!("${usd:.4}")
    } else if usd < 100.0 {
        format!("${usd:.2}")
    } else {
        format!("${usd:.0}")
    }
}

#[inline]
#[allow(clippy::cast_precision_loss)] // token counts are far below 2^53
const fn as_f64(v: u64) -> f64 {
    v as f64
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn price_lookup_longest_prefix_wins() {
        // claude-3-5-haiku should beat the broad "claude-" entry.
        let p = price_for("claude-3-5-haiku-20241022").unwrap();
        assert!((p.input_per_mtok - 0.8).abs() < f64::EPSILON);
        let opus = price_for("claude-opus-4-8").unwrap();
        assert!((opus.output_per_mtok - 75.0).abs() < f64::EPSILON);
    }

    #[test]
    fn price_lookup_strips_provider_prefix() {
        assert!(price_for("anthropic/claude-sonnet-4-6").is_some());
        assert!(price_for("openai:gpt-4o").is_some());
        assert!(price_for("totally-unknown-model").is_none());
    }

    #[test]
    fn cost_math_is_per_million() {
        let price = ModelPrice::cached(3.0, 15.0, 0.3, 3.75);
        // 1M input + 1M output -> $3 + $15.
        let c = cost_of(&price, &TokenUsage::new(1_000_000, 1_000_000, 0, 0));
        assert!((c.total() - 18.0).abs() < 1e-9);
    }

    #[test]
    fn microdollars_round_trip() {
        let price = ModelPrice::flat(2.5, 10.0);
        // 1000 output tokens at $10/Mtok = $0.01 = 10_000 microdollars.
        let c = cost_of(&price, &TokenUsage::new(0, 1_000, 0, 0));
        assert_eq!(c.microdollars(), 10_000);
    }

    #[test]
    fn meter_accumulates_and_breaks_down_by_model() {
        let mut m = CostMeter::new();
        m.record("claude-sonnet-4-6", TokenUsage::new(1_000, 500, 0, 0), 0);
        m.record("claude-sonnet-4-6", TokenUsage::new(2_000, 100, 0, 0), 1_000);
        m.record("claude-opus-4-8", TokenUsage::new(1_000, 1_000, 0, 0), 2_000);
        let ins = m.insights();
        assert_eq!(ins.turns, 3);
        assert_eq!(ins.per_model.len(), 2);
        // Opus is pricier per token, but sonnet has more turns; sorted by cost.
        assert_eq!(ins.usage.input, 4_000);
        assert!(ins.cost.total() > 0.0);
    }

    #[test]
    fn cold_cache_detected_after_ttl() {
        let mut m = CostMeter::new();
        let first = m.record("claude-sonnet-4-6", TokenUsage::new(1_000, 10, 0, 100), 0);
        assert!(first.cache_warm, "first turn is always warm");
        let warm = m.record(
            "claude-sonnet-4-6",
            TokenUsage::new(10, 10, 900, 0),
            PROMPT_CACHE_TTL_MS - 1,
        );
        assert!(warm.cache_warm);
        let cold = m.record(
            "claude-sonnet-4-6",
            TokenUsage::new(1_000, 10, 0, 100),
            PROMPT_CACHE_TTL_MS * 3,
        );
        assert!(!cold.cache_warm, "gap beyond TTL is cold");
        assert_eq!(m.insights().cold_cache_turns, 1);
    }

    #[test]
    fn unpriced_model_reports_tokens_without_dollars() {
        let mut m = CostMeter::new();
        let t = m.record("some-future-model", TokenUsage::new(100, 50, 0, 0), 0);
        assert!(!t.priced);
        assert_eq!(t.cost.total(), 0.0);
        assert_eq!(t.usage.total(), 150);
    }

    #[test]
    fn cache_hit_rate_is_fraction_of_input() {
        let u = TokenUsage::new(100, 0, 300, 0);
        assert!((u.cache_hit_rate() - 0.75).abs() < 1e-9);
        assert_eq!(TokenUsage::default().cache_hit_rate(), 0.0);
    }

    #[test]
    fn usd_formatting_buckets() {
        assert_eq!(fmt_usd(0.0), "$0");
        assert_eq!(fmt_usd(0.0023), "$0.0023");
        assert_eq!(fmt_usd(1.42), "$1.42");
        assert_eq!(fmt_usd(128.4), "$128");
    }
}
