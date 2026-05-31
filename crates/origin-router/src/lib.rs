// SPDX-License-Identifier: Apache-2.0
//! Model routing strategies driven by fed-in health and latency signals.
//!
//! `origin` can drive many providers, but the baseline always sends a turn to
//! one fixed model. This crate adds pluggable routing so a session can split
//! work across models the way leading agents do: aider's architect/editor split
//! (a strong planner drafts, a cheap editor applies diffs), openclaude's
//! `SmartRouter` + per-agent routing, gemini-cli's phase-aware auto-routing
//! (heavy model for planning, fast model for the rest), and kilocode's Virtual
//! Quota Fallback (walk a chain, skipping exhausted models).
//!
//! It is pure logic — no network. Latency and error signals are fed in via
//! [`Router::record_result`] and folded into an exponential moving average, so
//! the crate is trivially testable and free of I/O concerns.
//!
//! ```
//! use origin_router::{ModelRef, Phase, Router, Strategy};
//!
//! let plan = ModelRef::new("anthropic", "claude-opus-4");
//! let fast = ModelRef::new("anthropic", "claude-haiku-4");
//! let router = Router::new(Strategy::PhaseAware {
//!     plan: plan.clone(),
//!     fast: fast.clone(),
//! });
//! assert_eq!(router.choose(Phase::Plan, &[]), Some(plan));
//! assert_eq!(router.choose(Phase::Edit, &[]), Some(fast));
//! ```

#![forbid(unsafe_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Smoothing factor for the latency / error-rate exponential moving average.
///
/// Higher values track recent samples more aggressively; `0.3` keeps a stable
/// signal while still reacting to a model that suddenly slows down or starts
/// erroring.
pub const EMA_ALPHA: f64 = 0.3;

/// Coarse classification of what a turn is doing, used by phase-aware routing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Phase {
    /// Planning / architecting: pick the strongest model.
    Plan,
    /// Editing existing code: apply diffs with a cheaper, faster model.
    Edit,
    /// Executing tools / running commands.
    Execute,
    /// No specific phase; treat as the non-plan default.
    #[default]
    Default,
}

/// A provider + model pair, the unit a strategy routes to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelRef {
    /// Provider identifier, e.g. `anthropic`, `openai`, `gemini`.
    pub provider: String,
    /// Model identifier within the provider, e.g. `claude-opus-4`.
    pub model: String,
}

impl ModelRef {
    /// Construct a model reference.
    #[must_use]
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }

    /// Stable map key in the form `provider/model`.
    ///
    /// Used to associate [`Health`] with a model across calls.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

/// How a [`Router`] selects a model for a turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Strategy {
    /// Always route to a single model.
    Fixed(ModelRef),
    /// Aider-style split: the architect drafts during [`Phase::Plan`], the
    /// editor handles every other phase.
    ArchitectEditor {
        /// Strong model used while planning.
        architect: ModelRef,
        /// Cheaper model used for edits / execution.
        editor: ModelRef,
    },
    /// Gemini-style phase-aware routing: `plan` for [`Phase::Plan`], `fast`
    /// otherwise.
    PhaseAware {
        /// Heavy model for planning.
        plan: ModelRef,
        /// Fast model for everything else.
        fast: ModelRef,
    },
    /// openclaude-style `SmartRouter`: rank the supplied candidates by health.
    Scored,
    /// kilocode-style Virtual Quota Fallback: walk the chain and return the
    /// first model that is not exhausted.
    QuotaFallback {
        /// Ordered preference list; earlier entries win when available.
        chain: Vec<ModelRef>,
    },
}

/// Per-model health signals, keyed in the router by [`ModelRef::key`].
///
/// Latency and error rate are exponential moving averages updated by
/// [`Router::record_result`]; `cost_rank` and `exhausted` are caller-managed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Health {
    /// EMA of observed request latency in milliseconds.
    pub ema_latency_ms: f64,
    /// EMA of the error rate in `[0, 1]` (1.0 == always failing).
    pub ema_error_rate: f64,
    /// Relative cost tier; lower is cheaper. Used as a tiebreak when scoring.
    pub cost_rank: u32,
    /// `true` when the model has hit a quota / rate limit and should be skipped.
    pub exhausted: bool,
}

impl Default for Health {
    fn default() -> Self {
        Self {
            ema_latency_ms: 0.0,
            ema_error_rate: 0.0,
            cost_rank: 0,
            exhausted: false,
        }
    }
}

impl Health {
    /// Fresh health for a model with no observations yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one observation into the EMAs.
    ///
    /// The first observation seeds the latency average directly so a single
    /// sample is not diluted toward zero; later samples blend with [`EMA_ALPHA`].
    fn observe(&mut self, latency_ms: u64, ok: bool, seeded: bool) {
        let sample_latency = as_f64(latency_ms);
        let sample_err = if ok { 0.0 } else { 1.0 };
        if seeded {
            self.ema_latency_ms = EMA_ALPHA.mul_add(sample_latency, (1.0 - EMA_ALPHA) * self.ema_latency_ms);
            self.ema_error_rate = EMA_ALPHA.mul_add(sample_err, (1.0 - EMA_ALPHA) * self.ema_error_rate);
        } else {
            self.ema_latency_ms = sample_latency;
            self.ema_error_rate = sample_err;
        }
    }

    /// Routing score: higher is better. Defined as `(1 - error) / latency`, so
    /// low error and low latency both raise the score. A floor keeps the
    /// division well-defined before any latency has been observed.
    #[must_use]
    pub fn score(&self) -> f64 {
        let latency = self.ema_latency_ms.max(1.0);
        (1.0 - self.ema_error_rate.clamp(0.0, 1.0)) / latency
    }
}

/// Errors that can arise constructing or driving a [`Router`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RouterError {
    /// A strategy that requires a non-empty chain was given an empty one.
    #[error("QuotaFallback strategy requires a non-empty chain")]
    EmptyChain,
}

/// Routes turns to models according to a [`Strategy`], tracking [`Health`].
#[derive(Debug, Clone)]
pub struct Router {
    strategy: Strategy,
    health: HashMap<String, Health>,
}

impl Router {
    /// Construct a router for `strategy` with empty health state.
    #[must_use]
    pub fn new(strategy: Strategy) -> Self {
        Self {
            strategy,
            health: HashMap::new(),
        }
    }

    /// Construct a router, rejecting structurally invalid strategies.
    ///
    /// # Errors
    ///
    /// Returns [`RouterError::EmptyChain`] when given a
    /// [`Strategy::QuotaFallback`] with an empty `chain`.
    pub fn try_new(strategy: Strategy) -> Result<Self, RouterError> {
        if let Strategy::QuotaFallback { chain } = &strategy {
            if chain.is_empty() {
                return Err(RouterError::EmptyChain);
            }
        }
        Ok(Self::new(strategy))
    }

    /// The strategy this router applies.
    #[must_use]
    pub const fn strategy(&self) -> &Strategy {
        &self.strategy
    }

    /// Current health for `m`, if any has been recorded.
    #[must_use]
    pub fn health(&self, m: &ModelRef) -> Option<&Health> {
        self.health.get(&m.key())
    }

    /// Fold a completed request for `m` into its latency / error EMAs.
    ///
    /// `latency_ms` is the observed round-trip time; `ok` is `false` for a
    /// failed or errored request. The first observation seeds the average; later
    /// ones blend with [`EMA_ALPHA`].
    pub fn record_result(&mut self, m: &ModelRef, latency_ms: u64, ok: bool) {
        let key = m.key();
        let seeded = self.health.contains_key(&key);
        let entry = self.health.entry(key).or_default();
        entry.observe(latency_ms, ok, seeded);
    }

    /// Mark `m` as exhausted (quota / rate limited) so fallback skips it.
    pub fn mark_exhausted(&mut self, m: &ModelRef) {
        self.health.entry(m.key()).or_default().exhausted = true;
    }

    /// Clear the exhausted flag for `m`, making it eligible again.
    pub fn clear_exhausted(&mut self, m: &ModelRef) {
        if let Some(h) = self.health.get_mut(&m.key()) {
            h.exhausted = false;
        }
    }

    /// Set the cost rank for `m` (lower is cheaper); used as a scoring tiebreak.
    pub fn set_cost_rank(&mut self, m: &ModelRef, rank: u32) {
        self.health.entry(m.key()).or_default().cost_rank = rank;
    }

    /// Whether `m` is currently flagged exhausted.
    #[must_use]
    pub fn is_exhausted(&self, m: &ModelRef) -> bool {
        self.health.get(&m.key()).is_some_and(|h| h.exhausted)
    }

    /// Choose a model for `phase` from `candidates`, applying the strategy.
    ///
    /// - [`Strategy::Fixed`] always returns its model (ignores `candidates`).
    /// - [`Strategy::ArchitectEditor`] returns the architect for [`Phase::Plan`],
    ///   the editor otherwise (ignores `candidates`).
    /// - [`Strategy::PhaseAware`] returns `plan` for [`Phase::Plan`], `fast`
    ///   otherwise (ignores `candidates`).
    /// - [`Strategy::QuotaFallback`] returns the first non-exhausted model in
    ///   its chain (ignores `candidates`).
    /// - [`Strategy::Scored`] ranks `candidates` by [`Health::score`] with
    ///   `cost_rank` as a tiebreak, skipping exhausted models; returns `None`
    ///   when `candidates` is empty or all are exhausted.
    #[must_use]
    pub fn choose(&self, phase: Phase, candidates: &[ModelRef]) -> Option<ModelRef> {
        match &self.strategy {
            Strategy::Fixed(m) => Some(m.clone()),
            Strategy::ArchitectEditor { architect, editor } => Some(if phase == Phase::Plan {
                architect.clone()
            } else {
                editor.clone()
            }),
            Strategy::PhaseAware { plan, fast } => Some(if phase == Phase::Plan {
                plan.clone()
            } else {
                fast.clone()
            }),
            Strategy::QuotaFallback { chain } => {
                chain.iter().find(|m| !self.is_exhausted(m)).cloned()
            }
            Strategy::Scored => self.best_scored(candidates),
        }
    }

    /// Rank `candidates` by health, returning the best non-exhausted one.
    fn best_scored(&self, candidates: &[ModelRef]) -> Option<ModelRef> {
        candidates
            .iter()
            .filter(|m| !self.is_exhausted(m))
            .max_by(|a, b| self.score_cmp(a, b))
            .cloned()
    }

    /// Compare two candidates the way [`Strategy::Scored`] does: higher
    /// [`Health::score`] wins, with a lower `cost_rank` as the tiebreak.
    fn score_cmp(&self, a: &ModelRef, b: &ModelRef) -> std::cmp::Ordering {
        let ha = self.health(a).copied().unwrap_or_default();
        let hb = self.health(b).copied().unwrap_or_default();
        ha.score()
            .partial_cmp(&hb.score())
            .unwrap_or(std::cmp::Ordering::Equal)
            // Lower cost_rank is better, so reverse the comparison to keep
            // "greater is better" for `max_by`.
            .then(hb.cost_rank.cmp(&ha.cost_rank))
    }

    /// Rank every non-exhausted candidate best-first by [`Strategy::Scored`].
    ///
    /// Unlike [`Router::choose`], which returns only the single best model,
    /// this returns the full ordering so callers can present a ranked list.
    /// The order is the same one `choose` would pick from: higher
    /// [`Health::score`] first, `cost_rank` breaking ties. Exhausted models
    /// are dropped. The comparison is independent of the configured strategy,
    /// so this works regardless of how the router was constructed.
    #[must_use]
    pub fn scored_order(&self, candidates: &[ModelRef]) -> Vec<ModelRef> {
        let mut ranked: Vec<ModelRef> =
            candidates.iter().filter(|m| !self.is_exhausted(m)).cloned().collect();
        ranked.sort_by(|a, b| self.score_cmp(b, a));
        ranked
    }
}

/// Rank `candidates` by measured latency via the [`Strategy::Scored`] health
/// model, best (lowest latency) first.
///
/// Each entry in `samples` pairs a candidate with its measured round-trip
/// latency in milliseconds; a candidate absent from `samples` keeps the
/// default (zero-latency) health and therefore sorts ahead of any measured
/// one — callers that want measured models to win should supply a sample for
/// every candidate. This is the pure ranking helper behind the local-Ollama
/// latency fold in `origin providers recommend`: it does no I/O, so the
/// latency-to-order mapping is unit-testable without a live server.
#[must_use]
pub fn rank_by_latency(samples: &[(ModelRef, u64)]) -> Vec<ModelRef> {
    let mut router = Router::new(Strategy::Scored);
    for (m, latency_ms) in samples {
        router.record_result(m, *latency_ms, true);
    }
    let candidates: Vec<ModelRef> = samples.iter().map(|(m, _)| m.clone()).collect();
    router.scored_order(&candidates)
}

#[inline]
#[allow(clippy::cast_precision_loss)] // latency samples are far below 2^53
const fn as_f64(v: u64) -> f64 {
    v as f64
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    fn m(provider: &str, model: &str) -> ModelRef {
        ModelRef::new(provider, model)
    }

    #[test]
    fn architect_editor_picks_per_phase() {
        let architect = m("anthropic", "claude-opus-4");
        let editor = m("anthropic", "claude-haiku-4");
        let r = Router::new(Strategy::ArchitectEditor {
            architect: architect.clone(),
            editor: editor.clone(),
        });
        assert_eq!(r.choose(Phase::Plan, &[]), Some(architect));
        assert_eq!(r.choose(Phase::Edit, &[]), Some(editor.clone()));
        assert_eq!(r.choose(Phase::Execute, &[]), Some(editor.clone()));
        assert_eq!(r.choose(Phase::Default, &[]), Some(editor));
    }

    #[test]
    fn phase_aware_picks_plan_then_fast() {
        let plan = m("gemini", "gemini-2.5-pro");
        let fast = m("gemini", "gemini-2.5-flash");
        let r = Router::new(Strategy::PhaseAware {
            plan: plan.clone(),
            fast: fast.clone(),
        });
        assert_eq!(r.choose(Phase::Plan, &[]), Some(plan));
        assert_eq!(r.choose(Phase::Execute, &[]), Some(fast));
    }

    #[test]
    fn fixed_always_returns_its_model() {
        let only = m("openai", "gpt-4o");
        let r = Router::new(Strategy::Fixed(only.clone()));
        assert_eq!(r.choose(Phase::Plan, &[]), Some(only.clone()));
        // Candidates are ignored entirely.
        assert_eq!(r.choose(Phase::Edit, &[m("x", "y")]), Some(only));
    }

    #[test]
    fn quota_fallback_skips_exhausted_and_recovers_after_clear() {
        let a = m("anthropic", "claude-opus-4");
        let b = m("openai", "gpt-4o");
        let c = m("gemini", "gemini-2.5-pro");
        let mut r = Router::new(Strategy::QuotaFallback {
            chain: vec![a.clone(), b.clone(), c.clone()],
        });
        // All fresh: first in chain wins.
        assert_eq!(r.choose(Phase::Default, &[]), Some(a.clone()));
        // Exhaust the first two: third wins.
        r.mark_exhausted(&a);
        r.mark_exhausted(&b);
        assert_eq!(r.choose(Phase::Default, &[]), Some(c.clone()));
        // All exhausted: None.
        r.mark_exhausted(&c);
        assert_eq!(r.choose(Phase::Default, &[]), None);
        // Recover the head of the chain: it wins again.
        r.clear_exhausted(&a);
        assert_eq!(r.choose(Phase::Default, &[]), Some(a));
    }

    #[test]
    fn scored_prefers_low_latency_low_error() {
        let slow = m("p", "slow");
        let fast = m("p", "fast");
        let mut r = Router::new(Strategy::Scored);
        // slow: high latency; fast: low latency, both reliable.
        r.record_result(&slow, 2_000, true);
        r.record_result(&fast, 100, true);
        assert_eq!(
            r.choose(Phase::Default, &[slow.clone(), fast.clone()]),
            Some(fast.clone())
        );
        // Now make fast start erroring; slow should win on (1-error) factor even
        // though it is slower, once fast's error rate climbs enough.
        for _ in 0..10 {
            r.record_result(&fast, 100, false);
        }
        assert_eq!(r.choose(Phase::Default, &[slow.clone(), fast]), Some(slow));
    }

    #[test]
    fn scored_uses_cost_rank_as_tiebreak() {
        let cheap = m("p", "cheap");
        let pricey = m("p", "pricey");
        let mut r = Router::new(Strategy::Scored);
        // Identical latency/error -> equal score; cheaper cost_rank wins.
        r.record_result(&cheap, 500, true);
        r.record_result(&pricey, 500, true);
        r.set_cost_rank(&cheap, 1);
        r.set_cost_rank(&pricey, 5);
        assert_eq!(
            r.choose(Phase::Default, &[pricey, cheap.clone()]),
            Some(cheap)
        );
    }

    #[test]
    fn scored_skips_exhausted_candidates() {
        let a = m("p", "a");
        let b = m("p", "b");
        let mut r = Router::new(Strategy::Scored);
        r.record_result(&a, 100, true); // a is better
        r.record_result(&b, 900, true);
        r.mark_exhausted(&a);
        // a would win on score but is exhausted, so b is chosen.
        assert_eq!(r.choose(Phase::Default, &[a, b.clone()]), Some(b));
    }

    #[test]
    fn scored_empty_candidates_is_none() {
        let r = Router::new(Strategy::Scored);
        assert_eq!(r.choose(Phase::Plan, &[]), None);
        assert_eq!(r.choose(Phase::Default, &[]), None);
    }

    #[test]
    fn ema_moves_toward_samples() {
        let x = m("p", "x");
        let mut r = Router::new(Strategy::Scored);
        // First sample seeds the EMA directly.
        r.record_result(&x, 1_000, true);
        let h1 = *r.health(&x).unwrap();
        assert_eq!(h1.ema_latency_ms, 1_000.0);
        assert_eq!(h1.ema_error_rate, 0.0);
        // A lower-latency, failing sample pulls latency down and error up, but
        // not all the way (blended by alpha).
        r.record_result(&x, 0, false);
        let h2 = *r.health(&x).unwrap();
        assert!(h2.ema_latency_ms < h1.ema_latency_ms);
        assert!(h2.ema_latency_ms > 0.0, "EMA should not jump fully to sample");
        assert!(h2.ema_error_rate > 0.0 && h2.ema_error_rate < 1.0);
        // Expected: 0.3*0 + 0.7*1000 = 700; 0.3*1 + 0.7*0 = 0.3.
        assert!((h2.ema_latency_ms - 700.0).abs() < 1e-9);
        assert!((h2.ema_error_rate - 0.3).abs() < 1e-9);
    }

    #[test]
    fn mark_and_clear_exhausted_roundtrip() {
        let x = m("p", "x");
        let mut r = Router::new(Strategy::Scored);
        assert!(!r.is_exhausted(&x));
        r.mark_exhausted(&x);
        assert!(r.is_exhausted(&x));
        r.clear_exhausted(&x);
        assert!(!r.is_exhausted(&x));
        // Clearing a never-seen model is a no-op, not a panic.
        r.clear_exhausted(&m("never", "seen"));
    }

    #[test]
    fn try_new_rejects_empty_chain() {
        let err = Router::try_new(Strategy::QuotaFallback { chain: vec![] }).unwrap_err();
        assert_eq!(err, RouterError::EmptyChain);
        assert!(Router::try_new(Strategy::Scored).is_ok());
        assert!(
            Router::try_new(Strategy::QuotaFallback {
                chain: vec![m("p", "a")]
            })
            .is_ok()
        );
    }

    #[test]
    fn model_ref_key_is_provider_slash_model() {
        assert_eq!(m("anthropic", "claude-opus-4").key(), "anthropic/claude-opus-4");
    }

    #[test]
    fn scored_order_ranks_all_candidates_best_first() {
        let slow = m("p", "slow");
        let mid = m("p", "mid");
        let fast = m("p", "fast");
        let mut r = Router::new(Strategy::Scored);
        r.record_result(&slow, 2_000, true);
        r.record_result(&mid, 800, true);
        r.record_result(&fast, 100, true);
        let order = r.scored_order(&[slow.clone(), mid.clone(), fast.clone()]);
        assert_eq!(order, vec![fast, mid, slow]);
    }

    #[test]
    fn scored_order_drops_exhausted() {
        let a = m("p", "a");
        let b = m("p", "b");
        let mut r = Router::new(Strategy::Scored);
        r.record_result(&a, 100, true);
        r.record_result(&b, 900, true);
        r.mark_exhausted(&a);
        // a would lead on score but is exhausted, so only b remains.
        assert_eq!(r.scored_order(&[a, b.clone()]), vec![b]);
    }

    #[test]
    fn rank_by_latency_orders_lowest_first() {
        let slow = m("ollama", "llama3.1:70b");
        let fast = m("ollama", "llama3.2");
        // fast has the lower measured latency, so it must lead.
        let order = rank_by_latency(&[(slow.clone(), 1_800), (fast.clone(), 120)]);
        assert_eq!(order, vec![fast, slow]);
    }

    #[test]
    fn rank_by_latency_empty_is_empty() {
        assert!(rank_by_latency(&[]).is_empty());
    }

    #[test]
    fn rank_by_latency_single_sample_round_trips() {
        let only = m("ollama", "qwen2.5-coder");
        assert_eq!(rank_by_latency(&[(only.clone(), 250)]), vec![only]);
    }

    #[test]
    fn strategy_accessor_and_serde_roundtrip() {
        let s = Strategy::PhaseAware {
            plan: m("a", "p"),
            fast: m("a", "f"),
        };
        let r = Router::new(s.clone());
        assert_eq!(r.strategy(), &s);
        let json = serde_json::to_string(&s).unwrap();
        let back: Strategy = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}
