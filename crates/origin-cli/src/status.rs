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

/// Whether the opt-in per-turn cost line is enabled (`ORIGIN_TURN_COST=1`).
///
/// Default-off: when the env var is unset (or not `1`) this is `false`, so the
/// caller emits no extra line and the turn output stays byte-identical.
#[must_use]
pub fn turn_cost_enabled() -> bool {
    std::env::var("ORIGIN_TURN_COST").as_deref() == Ok("1")
}

/// The localized "this turn cost" line for a single turn's USD figure, or `None`
/// when the figure is zero/negative (an unpriced model or no spend) so we never
/// print a misleading `$0.00`.
///
/// Routes through the `cost.turn` catalog key ("This turn cost {usd}" in
/// English), so the per-turn line localizes with `--lang`/`$LANG`. Gated by
/// [`turn_cost_enabled`] at the call site (default-off ⇒ byte-identical).
#[must_use]
pub fn format_turn_cost(turn_usd: f64) -> Option<String> {
    if turn_usd <= 0.0 {
        return None;
    }
    Some(crate::locale::linef(
        "cost.turn",
        &[("usd", &origin_cost::fmt_usd(turn_usd))],
    ))
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
        assert!(
            line.len() > usd.len(),
            "a catalog template wraps the figure: {line}"
        );
    }

    #[test]
    fn format_turn_cost_routes_through_cost_turn_catalog_key() {
        // Zero / negative spend prints nothing.
        assert!(format_turn_cost(0.0).is_none());
        assert!(format_turn_cost(-0.5).is_none());
        // A real figure renders through the `cost.turn` key. Assert locale-robustly
        // (the test binary is shared; another test may have pinned a non-English
        // override on the process-global OnceLock): the output must equal the
        // localized `cost.turn` template with the figure substituted — a wrong key
        // (e.g. `cost.session`) renders different text and fails this.
        let usd = origin_cost::fmt_usd(0.0042);
        let line = format_turn_cost(0.0042).expect("nonzero figure renders a line");
        assert_eq!(
            line,
            crate::locale::linef("cost.turn", &[("usd", &usd)]),
            "must route through the cost.turn catalog key"
        );
        assert!(line.contains(&usd), "must embed the formatted usd: {line}");
        assert!(
            line.len() > usd.len(),
            "a catalog template wraps the figure: {line}"
        );
    }
}
