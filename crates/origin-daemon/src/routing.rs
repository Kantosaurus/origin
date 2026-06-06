// SPDX-License-Identifier: Apache-2.0
//! Live, per-turn model routing wired onto the agent loop.
//!
//! [`origin_router`] is pure logic: given a [`Phase`](origin_router::Phase) and
//! a set of candidates it picks a [`ModelRef`](origin_router::ModelRef). This
//! module turns that helper into a *live* router that the daemon consults on
//! every turn of [`run_loop`](crate::agent::run_loop):
//!
//! * It owns the [`Router`](origin_router::Router) behind a `Mutex` so latency /
//!   error health (the `Scored` EMA) and quota-exhaustion (the `QuotaFallback`
//!   chain) accumulate **across prompts**, not just within one `run_loop` call.
//! * It classifies each turn into a [`Phase`](origin_router::Phase) — turn 1 is
//!   `Plan` (the model is deciding what to do), later turns are `Edit` (applying
//!   tool results) — so aider's architect/editor split and gemini's phase-aware
//!   Pro/Flash routing happen turn-by-turn.
//! * The zero-cost path [`choose_model`](LiveRouter::choose_model) overrides the
//!   turn's model **only** when the chosen
//!   [`ModelRef`](origin_router::ModelRef) field `.provider` matches the **active**
//!   provider the loop already holds (`run_loop` borrows one `&dyn Provider`).
//! * Cross-provider picks are no longer pre-emptively dropped: the lower-level
//!   [`choose_model_ref`](LiveRouter::choose_model_ref) surfaces the full
//!   [`ModelRef`](origin_router::ModelRef) (provider + model) so the agent loop
//!   can decide whether to rebuild a provider for that turn (see
//!   [`crate::provider_factory::build_provider_for`]). When no factory is
//!   reachable the loop still falls back to the active provider, so behaviour is
//!   unchanged for the same-provider case.
//!
//! Activation is entirely opt-in via the `ORIGIN_ROUTER` environment variable.
//! When it is unset, [`global`] returns `None`, [`LoopOptions.router`] stays
//! `None`, and the agent loop reads `session.model` every turn exactly as
//! before — the wire is byte-identical.
//!
//! Recognised `ORIGIN_ROUTER` values and their companion variables:
//!
//! | `ORIGIN_ROUTER` | Strategy | Companion env (`provider/model`) |
//! |---|---|---|
//! | `auto` | [`PhaseAware`](origin_router::Strategy::PhaseAware) | none required — catalog defaults; `ORIGIN_ROUTER_PLAN`/`ORIGIN_ROUTER_FAST` override either leg |
//! | `phase` | [`PhaseAware`](origin_router::Strategy::PhaseAware) | `ORIGIN_ROUTER_PLAN`, `ORIGIN_ROUTER_FAST` |
//! | `architect` | [`ArchitectEditor`](origin_router::Strategy::ArchitectEditor) | `ORIGIN_ROUTER_PLAN`, `ORIGIN_ROUTER_FAST` |
//! | `scored` | [`Scored`](origin_router::Strategy::Scored) | `ORIGIN_ROUTER_CANDIDATES` (comma-separated) |
//! | `quota` | [`QuotaFallback`](origin_router::Strategy::QuotaFallback) | `ORIGIN_ROUTER_CHAIN` (comma-separated) |
//!
//! `auto` is the zero-config sentinel: unlike `phase` (which is inert without
//! its companion vars) it always builds, defaulting plan/fast to the latest
//! Anthropic pair (the catalog's default provider). Set `ORIGIN_ROUTER_PLAN` /
//! `ORIGIN_ROUTER_FAST` (`provider/model`) to retarget either leg.

use std::sync::{Arc, Mutex, OnceLock};

use origin_router::{ModelRef, Phase, Router, Strategy};

/// A daemon-wide, thread-safe live router consulted on every turn.
#[derive(Debug)]
pub struct LiveRouter {
    /// The pure router, guarded so health/quota state survives across prompts.
    /// The lock is only ever held for the duration of a synchronous `choose` /
    /// `record_result` call — never across an `.await` — so it cannot stall the
    /// async runtime.
    inner: Mutex<Router>,
    /// Candidate models for the `Scored` strategy. The other strategies ignore
    /// candidates (they encode their own model set), so this is empty for them.
    candidates: Vec<ModelRef>,
}

impl LiveRouter {
    /// Build a live router for an explicit [`Strategy`] (used by tests and the
    /// env constructor). `candidates` matters only for [`Strategy::Scored`].
    #[must_use]
    pub fn new(strategy: Strategy, candidates: Vec<ModelRef>) -> Self {
        Self {
            inner: Mutex::new(Router::new(strategy)),
            candidates,
        }
    }

    /// Pick the full [`ModelRef`] for `turn` — provider **and** model — without
    /// any same-provider filtering. `None` ⇒ the strategy yielded nothing (an
    /// exhausted `QuotaFallback` chain or empty `Scored` candidates).
    ///
    /// This is the cross-provider-aware primitive: a pick whose `provider`
    /// differs from the active one is *surfaced* here (it is no longer dropped),
    /// so the agent loop can rebuild a provider for that turn. Same-provider
    /// callers that only want the zero-cost override should use
    /// [`choose_model`](Self::choose_model) instead.
    ///
    /// Turn 1 is classified [`Phase::Plan`]; every later turn [`Phase::Edit`].
    #[must_use]
    pub fn choose_model_ref(&self, turn: u32) -> Option<ModelRef> {
        let phase = if turn == 1 { Phase::Plan } else { Phase::Edit };
        self.inner.lock().ok()?.choose(phase, &self.candidates)
    }

    /// Pick the model for `turn`, returning the model id **only** when the
    /// chosen provider matches `active_provider`. `None` ⇒ the caller keeps
    /// `session.model` (cross-provider pick, exhausted chain, or empty
    /// `Scored` candidates).
    ///
    /// Convenience wrapper over [`choose_model_ref`](Self::choose_model_ref)
    /// that preserves the original zero-cost same-provider override path.
    ///
    /// Turn 1 is classified [`Phase::Plan`]; every later turn [`Phase::Edit`].
    #[must_use]
    pub fn choose_model(&self, turn: u32, active_provider: &str) -> Option<String> {
        let chosen = self.choose_model_ref(turn)?;
        (chosen.provider == active_provider).then_some(chosen.model)
    }

    /// Fold a completed turn's latency / success into the router's EMA health.
    /// A successful call also clears any prior exhaustion flag so a model that
    /// recovers from a rate-limit becomes eligible again (self-healing
    /// quota-fallback).
    pub fn record(&self, provider: &str, model: &str, latency_ms: u64, ok: bool) {
        if let Ok(mut r) = self.inner.lock() {
            let m = ModelRef::new(provider, model);
            r.record_result(&m, latency_ms, ok);
            if ok {
                r.clear_exhausted(&m);
            }
        }
    }

    /// Flag `provider/model` exhausted so a [`Strategy::QuotaFallback`] chain
    /// skips it on the next turn / prompt. Cleared automatically by the next
    /// successful [`record`](Self::record) for that model.
    pub fn mark_exhausted(&self, provider: &str, model: &str) {
        if let Ok(mut r) = self.inner.lock() {
            r.mark_exhausted(&ModelRef::new(provider, model));
        }
    }

    /// Construct a router from the `ORIGIN_ROUTER` environment, or `None` when
    /// unset / malformed (routing simply stays off — never an error).
    #[must_use]
    pub fn from_env() -> Option<Arc<Self>> {
        let kind = std::env::var("ORIGIN_ROUTER").ok()?;
        Self::from_env_with(&kind, |k| std::env::var(k).ok()).map(Arc::new)
    }

    /// Testable core of [`from_env`](Self::from_env): `kind` is the
    /// `ORIGIN_ROUTER` value, `get` resolves a companion variable by name.
    fn from_env_with(kind: &str, get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        match kind.trim() {
            "auto" => {
                // Zero-config phase-aware routing: a capable model to plan, a
                // cheap one to edit. Defaults to the latest Anthropic pair (the
                // catalog's default provider); ORIGIN_ROUTER_PLAN/FAST override
                // either leg, so `auto` retargets to any provider without the
                // user learning the strategy names. ALWAYS builds (unlike
                // "phase", which is None without companions) — the sentinel
                // "just works".
                let plan = get("ORIGIN_ROUTER_PLAN")
                    .as_deref()
                    .and_then(parse_model_ref)
                    .unwrap_or_else(default_plan_model);
                let fast = get("ORIGIN_ROUTER_FAST")
                    .as_deref()
                    .and_then(parse_model_ref)
                    .unwrap_or_else(default_fast_model);
                Some(Self::new(Strategy::PhaseAware { plan, fast }, Vec::new()))
            }
            "phase" | "architect" => {
                let plan = parse_model_ref(&get("ORIGIN_ROUTER_PLAN")?)?;
                let fast = parse_model_ref(&get("ORIGIN_ROUTER_FAST")?)?;
                let strategy = if kind.trim() == "phase" {
                    Strategy::PhaseAware { plan, fast }
                } else {
                    Strategy::ArchitectEditor {
                        architect: plan,
                        editor: fast,
                    }
                };
                Some(Self::new(strategy, Vec::new()))
            }
            "scored" => {
                let candidates = parse_model_list(&get("ORIGIN_ROUTER_CANDIDATES")?);
                if candidates.is_empty() {
                    return None;
                }
                Some(Self::new(Strategy::Scored, candidates))
            }
            "quota" => {
                let chain = parse_model_list(&get("ORIGIN_ROUTER_CHAIN")?);
                if chain.is_empty() {
                    return None;
                }
                Some(Self::new(Strategy::QuotaFallback { chain }, Vec::new()))
            }
            _ => None,
        }
    }
}

/// Default `ORIGIN_ROUTER=auto` "plan/architect" model (overridable via
/// `ORIGIN_ROUTER_PLAN`): the latest capable Anthropic model.
fn default_plan_model() -> ModelRef {
    ModelRef::new("anthropic", "claude-opus-4-8")
}

/// Default `ORIGIN_ROUTER=auto` "fast/editor" model (overridable via
/// `ORIGIN_ROUTER_FAST`): a cheap, quick Anthropic model.
fn default_fast_model() -> ModelRef {
    ModelRef::new("anthropic", "claude-haiku-4-5")
}

/// Parse a single `provider/model` reference. Splits on the first `/`; both
/// sides must be non-empty.
fn parse_model_ref(s: &str) -> Option<ModelRef> {
    let (provider, model) = s.trim().split_once('/')?;
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    Some(ModelRef::new(provider, model))
}

/// Parse a comma-separated list of `provider/model` references, skipping any
/// malformed entries.
fn parse_model_list(s: &str) -> Vec<ModelRef> {
    s.split(',').filter_map(parse_model_ref).collect()
}

/// Process-wide router, initialised once from the environment. Returns `None`
/// when `ORIGIN_ROUTER` is unset ⇒ routing is off and the agent loop is
/// byte-identical to before.
#[must_use]
pub fn global() -> Option<Arc<LiveRouter>> {
    static GLOBAL: OnceLock<Option<Arc<LiveRouter>>> = OnceLock::new();
    GLOBAL.get_or_init(LiveRouter::from_env).clone()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn phase_routing_picks_plan_then_fast_same_provider() {
        let lr = LiveRouter::new(
            Strategy::PhaseAware {
                plan: ModelRef::new("anthropic", "claude-opus-4"),
                fast: ModelRef::new("anthropic", "claude-haiku-4"),
            },
            Vec::new(),
        );
        // Turn 1 = Plan ⇒ opus; later turns = Edit ⇒ haiku. Both on the active
        // provider, so the model is overridden.
        assert_eq!(lr.choose_model(1, "anthropic").as_deref(), Some("claude-opus-4"));
        assert_eq!(lr.choose_model(2, "anthropic").as_deref(), Some("claude-haiku-4"));
    }

    #[test]
    fn cross_provider_pick_is_skipped_by_same_provider_helper() {
        let lr = LiveRouter::new(
            Strategy::Fixed(ModelRef::new("gemini", "gemini-2.5-pro")),
            Vec::new(),
        );
        // Active provider is anthropic but the strategy routes to gemini ⇒ the
        // same-provider helper must NOT override (it cannot reuse the borrowed
        // anthropic provider for a gemini model).
        assert_eq!(lr.choose_model(1, "anthropic"), None);
        // When the active provider matches, the override applies.
        assert_eq!(lr.choose_model(1, "gemini").as_deref(), Some("gemini-2.5-pro"));
    }

    #[test]
    fn cross_provider_pick_is_surfaced_by_choose_model_ref() {
        let lr = LiveRouter::new(
            Strategy::Fixed(ModelRef::new("gemini", "gemini-2.5-pro")),
            Vec::new(),
        );
        // The cross-provider-aware primitive surfaces the FULL pick — provider
        // and model — even though the active loop provider is anthropic. This is
        // what lets the agent loop rebuild a provider for the turn instead of
        // silently dropping the pick (the old pre-emptive filter is gone).
        let chosen = lr.choose_model_ref(1).expect("a Fixed strategy always picks");
        assert_eq!(chosen.provider, "gemini");
        assert_eq!(chosen.model, "gemini-2.5-pro");
    }

    #[test]
    fn quota_fallback_skips_exhausted_then_recovers_on_success() {
        let lr = LiveRouter::new(
            Strategy::QuotaFallback {
                chain: vec![ModelRef::new("anthropic", "a"), ModelRef::new("anthropic", "b")],
            },
            Vec::new(),
        );
        assert_eq!(lr.choose_model(1, "anthropic").as_deref(), Some("a"));
        lr.mark_exhausted("anthropic", "a");
        assert_eq!(lr.choose_model(1, "anthropic").as_deref(), Some("b"));
        // A later success for `a` clears its exhaustion ⇒ it wins again.
        lr.record("anthropic", "a", 100, true);
        assert_eq!(lr.choose_model(1, "anthropic").as_deref(), Some("a"));
    }

    #[test]
    fn scored_prefers_lower_latency_candidate() {
        let slow = ModelRef::new("anthropic", "slow");
        let fast = ModelRef::new("anthropic", "fast");
        let lr = LiveRouter::new(Strategy::Scored, vec![slow, fast]);
        lr.record("anthropic", "slow", 2_000, true);
        lr.record("anthropic", "fast", 100, true);
        assert_eq!(lr.choose_model(1, "anthropic").as_deref(), Some("fast"));
    }

    #[test]
    fn auto_router_uses_catalog_defaults_with_no_companions() {
        // The zero-config sentinel must build even with no companion env vars,
        // defaulting to phase-aware routing on the latest Anthropic pair.
        let lr = LiveRouter::from_env_with("auto", |_| None).expect("auto always builds");
        assert_eq!(
            lr.choose_model(1, "anthropic").as_deref(),
            Some("claude-opus-4-8")
        );
        assert_eq!(
            lr.choose_model(2, "anthropic").as_deref(),
            Some("claude-haiku-4-5")
        );
    }

    #[test]
    fn auto_router_honors_companion_overrides() {
        let env = |k: &str| match k {
            "ORIGIN_ROUTER_PLAN" => Some("openai/gpt-x".to_string()),
            "ORIGIN_ROUTER_FAST" => Some("openai/gpt-mini".to_string()),
            _ => None,
        };
        let lr = LiveRouter::from_env_with("auto", env).expect("auto builds with overrides");
        assert_eq!(lr.choose_model(1, "openai").as_deref(), Some("gpt-x"));
        assert_eq!(lr.choose_model(2, "openai").as_deref(), Some("gpt-mini"));
    }

    #[test]
    fn from_env_phase_builds_phase_aware() {
        let env = |k: &str| match k {
            "ORIGIN_ROUTER_PLAN" => Some("anthropic/opus".to_string()),
            "ORIGIN_ROUTER_FAST" => Some("anthropic/haiku".to_string()),
            _ => None,
        };
        let lr = LiveRouter::from_env_with("phase", env).unwrap();
        assert_eq!(lr.choose_model(1, "anthropic").as_deref(), Some("opus"));
        assert_eq!(lr.choose_model(3, "anthropic").as_deref(), Some("haiku"));
    }

    #[test]
    fn from_env_missing_companion_is_none() {
        // `phase` requires both PLAN and FAST; missing one ⇒ routing off.
        assert!(LiveRouter::from_env_with("phase", |_| None).is_none());
        // Unknown kind ⇒ None.
        assert!(LiveRouter::from_env_with("bogus", |_| Some("x/y".into())).is_none());
        // Empty scored candidates ⇒ None.
        assert!(LiveRouter::from_env_with("scored", |_| Some(String::new())).is_none());
    }

    #[test]
    fn parse_model_ref_handles_edges() {
        assert_eq!(
            parse_model_ref("anthropic/claude-opus-4"),
            Some(ModelRef::new("anthropic", "claude-opus-4"))
        );
        assert_eq!(parse_model_ref("noslash"), None);
        assert_eq!(parse_model_ref("/model"), None);
        assert_eq!(parse_model_ref("provider/"), None);
        // Model ids may themselves contain slashes (split_once keeps the rest).
        assert_eq!(
            parse_model_ref("bedrock/anthropic.claude/v2"),
            Some(ModelRef::new("bedrock", "anthropic.claude/v2"))
        );
    }
}
