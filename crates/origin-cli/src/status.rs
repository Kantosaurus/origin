//! Status line: live token + cost accounting.
//!
//! Pricing is per-model, per-1M-tokens, from a small in-source lookup table.
//! Unknown models cost zero — better than silently wrong numbers. Phase 8 will
//! externalize pricing.

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct UsageSnapshot {
    pub provider: &'static str,
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_input_tokens: u32,
    pub cache_creation_input_tokens: u32,
    pub elapsed: Duration,
}

impl UsageSnapshot {
    #[must_use]
    pub fn new(provider: &'static str, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            elapsed: Duration::ZERO,
        }
    }
}

/// Per-million-token USD prices: (input, output, `cache_read`, `cache_write`).
fn pricing(model: &str) -> (f64, f64, f64, f64) {
    match model {
        "claude-opus-4-7" => (15.00, 75.00, 1.50, 18.75),
        "claude-sonnet-4-6" => (3.00, 15.00, 0.30, 3.75),
        "claude-haiku-4-5" => (0.80, 4.00, 0.08, 1.00),
        _ => (0.0, 0.0, 0.0, 0.0),
    }
}

#[must_use]
#[allow(clippy::suboptimal_flops, clippy::similar_names)] // readability > FMA micro-optimization for status accounting
pub fn cost_usd(snap: &UsageSnapshot) -> f64 {
    let (pi, po, pcr, pcw) = pricing(&snap.model);
    let m = 1_000_000.0;
    f64::from(snap.input_tokens) / m * pi
        + f64::from(snap.output_tokens) / m * po
        + f64::from(snap.cache_read_input_tokens) / m * pcr
        + f64::from(snap.cache_creation_input_tokens) / m * pcw
}

#[must_use]
pub fn render_line(snap: &UsageSnapshot) -> String {
    let cost = cost_usd(snap);
    let secs = snap.elapsed.as_secs_f64();
    format!(
        "[{}/{}]  in {}  out {}  cache_r {}  cache_w {}  ${:.3}  {:.3}s",
        snap.provider,
        snap.model,
        snap.input_tokens,
        snap.output_tokens,
        snap.cache_read_input_tokens,
        snap.cache_creation_input_tokens,
        cost,
        secs,
    )
}
