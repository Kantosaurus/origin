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

/// The localized "session total" line for a cumulative USD figure, or `None`
/// when the figure is zero/negative (an unpriced model or no spend) so we never
/// print a misleading `$0.00`.
///
/// Routes through the `cost.session` catalog key ("Session total: {usd}" in
/// English), so the farewell summary localizes with `--lang`/`$LANG`.
#[must_use]
pub fn format_session_total(total_usd: f64) -> Option<String> {
    if total_usd <= 0.0 {
        return None;
    }
    Some(crate::locale::linef(
        "cost.session",
        &[("usd", &origin_cost::fmt_usd(total_usd))],
    ))
}

/// As [`format_session_total`], computing the cumulative cost from a snapshot.
#[must_use]
pub fn session_total_line(snap: &UsageSnapshot) -> Option<String> {
    format_session_total(cost_usd(snap))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn format_session_total_routes_through_cost_session_catalog_key() {
        // Zero / negative spend prints nothing (an unpriced model would
        // otherwise show a misleading $0.00 session total).
        assert!(format_session_total(0.0).is_none());
        assert!(format_session_total(-1.0).is_none());
        // A real figure renders. Assert locale-ROBUSTLY (the test binary is
        // shared and another test may have pinned a non-English locale override,
        // which is a process-global OnceLock that cannot be reset): the output
        // must equal the localized `cost.session` template with the figure
        // substituted — a wrong key (e.g. `cost.turn`) renders different text in
        // the active locale and fails this. We do not assume the English literal.
        let usd = origin_cost::fmt_usd(0.0123);
        let line = format_session_total(0.0123).expect("nonzero figure renders a line");
        assert_eq!(
            line,
            crate::locale::linef("cost.session", &[("usd", &usd)]),
            "must route through the cost.session catalog key"
        );
        assert!(line.contains(&usd), "must embed the formatted usd: {line}");
        assert!(line.len() > usd.len(), "a catalog template wraps the figure: {line}");
    }
}
