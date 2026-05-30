// SPDX-License-Identifier: Apache-2.0
//! Status line: live token + cost accounting.
//!
//! Pricing is delegated to [`origin_cost`], a centralized ~40-model table with
//! published prompt-cache read/write rates. Unknown models cost zero — better
//! than silently wrong numbers — and `origin_cost::price_for` returns `None`
//! so the line shows tokens without a misleading dollar figure.

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

/// Cumulative USD cost for `snap` under the centralized `origin_cost` pricing
/// table. Returns `0.0` for unpriced models.
#[must_use]
pub fn cost_usd(snap: &UsageSnapshot) -> f64 {
    let usage = origin_cost::TokenUsage::new(
        u64::from(snap.input_tokens),
        u64::from(snap.output_tokens),
        u64::from(snap.cache_read_input_tokens),
        u64::from(snap.cache_creation_input_tokens),
    );
    origin_cost::price_for(&snap.model).map_or(0.0, |p| origin_cost::cost_of(&p, &usage).total())
}

#[must_use]
pub fn render_line(snap: &UsageSnapshot) -> String {
    let cost = origin_cost::fmt_usd(cost_usd(snap));
    let secs = snap.elapsed.as_secs_f64();
    format!(
        "[{}/{}]  in {}  out {}  cache_r {}  cache_w {}  {}  {:.3}s",
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
