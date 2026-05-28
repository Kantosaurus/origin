//! Goal driver — after every `run_loop` return, decide whether to verify,
//! iterate, or clear the active goal. Per-connection.
//!
//! The novel mechanism: the main model self-tags every turn with a
//! `<goal-status>` outcome; the driver runs the Haiku verifier ONLY on
//! `Met` claims (at most once per goal). This avoids the
//! per-turn-full-transcript Haiku eval used by other CLIs and keeps the
//! token-cost-per-goal proportional to ~80 × N system-prompt tokens plus
//! one verifier call, instead of ~50k × N.

use crate::protocol::StreamEvent;
use origin_goal::verifier::{Verdict, Verifier, VerifierError};
use origin_goal::{ClearReason, ClearReasonWire, GoalState, TagOutcome, TagOutcomeWire};

/// What the connection task should do after handling the driver's decision.
#[derive(Debug)]
pub enum DriverDecision {
    /// Goal is over; emit `Cleared` event and drop the `GoalState`.
    Cleared {
        reason: ClearReasonWire,
        iter: u32,
        tokens_spent: u64,
    },
    /// Iterate again with this synthesized user prompt. The caller emits
    /// `iter_event` first so the CLI sees the per-iteration counter advance.
    Iterate {
        synthesized_prompt: String,
        iter_event: StreamEvent,
    },
}

/// Translate a `TagOutcome` + cap state into a [`DriverDecision`].
///
/// Caller responsibilities:
/// - Charge `LoopSummary` tokens to `state` via `record_iteration` BEFORE calling.
/// - On `Iterate`, send the `iter_event` to the client, then call `run_loop`
///   with `synthesized_prompt` as the user message.
/// - On `Cleared`, send a `GoalCleared` event and drop the state from the
///   connection.
///
/// The verifier is called by the driver only on `TagOutcome::Met`. We pass it
/// in as a trait object so tests can substitute a `MockVerifier`.
pub async fn drive(
    state: &mut GoalState,
    last_turn_text: &str,
    verifier: &dyn Verifier,
) -> DriverDecision {
    // Cap check first — never overshoot.
    if let Some(reason) = state.cap_check() {
        return cleared(state, reason);
    }
    let tag = state.last_status_tag.clone().unwrap_or(TagOutcome::Missing);
    match tag {
        TagOutcome::Met => {
            let truncated = truncate_for_verifier(last_turn_text);
            match verifier.verify(&state.condition, &truncated).await {
                Ok((Verdict::Met, in_tok, out_tok)) => {
                    state.record_verifier_tokens(in_tok, out_tok);
                    cleared(
                        state,
                        ClearReason::Met {
                            reason: "verifier confirmed".into(),
                        },
                    )
                }
                Ok((Verdict::NotMet { reason }, in_tok, out_tok)) => {
                    state.record_verifier_tokens(in_tok, out_tok);
                    iterate(
                        state,
                        format!(
                            "[goal-driver] You claimed the goal was met, but the verifier disagreed: \
                             {reason}. Address that specific gap and continue."
                        ),
                    )
                }
                Err(
                    VerifierError::RateLimit
                    | VerifierError::Transport(_)
                    | VerifierError::Malformed(_),
                ) => {
                    // Fail open — trust the main model's `met` claim rather
                    // than burn more budget on a verifier we can't reach.
                    cleared(state, ClearReason::VerifierUnavailable)
                }
            }
        }
        TagOutcome::InProgress { what_remains } => iterate(
            state,
            format!(
                "[goal-driver] Continue toward the active goal. What remains: {}",
                if what_remains.is_empty() {
                    "unspecified — keep going.".to_string()
                } else {
                    what_remains
                }
            ),
        ),
        TagOutcome::Missing => iterate(
            state,
            "[goal-driver] Continue toward the active goal. What remains: \
             unknown — main model did not emit a <goal-status> tag last turn; \
             emit one this turn."
                .to_string(),
        ),
        TagOutcome::Blocked { why } => iterate(
            state,
            format!(
                "[goal-driver] Last turn reported the goal blocked: {why}. \
                 Either resolve the blocker yourself, or if it truly requires the human, \
                 restate the blocker clearly and end the turn — the driver will then \
                 clear the goal so the user can respond."
            ),
        ),
    }
}

fn cleared(state: &GoalState, reason: ClearReason) -> DriverDecision {
    DriverDecision::Cleared {
        reason: reason.into(),
        iter: state.iter,
        tokens_spent: state.tokens_spent,
    }
}

fn iterate(state: &GoalState, prompt: String) -> DriverDecision {
    let iter_event = StreamEvent::GoalIteration {
        iter: state.iter,
        tokens_spent: state.tokens_spent,
        last_tag: TagOutcomeWire::from(
            state.last_status_tag.clone().unwrap_or(TagOutcome::Missing),
        ),
    };
    DriverDecision::Iterate {
        synthesized_prompt: prompt,
        iter_event,
    }
}

const VERIFIER_INPUT_MAX_CHARS: usize = 4_000;

/// Truncate `s` from the LEFT (keep the tail) so the verifier sees the most
/// recent text in the assistant's final turn. Respects UTF-8 boundaries so a
/// multi-byte codepoint never gets bisected.
fn truncate_for_verifier(s: &str) -> String {
    if s.len() <= VERIFIER_INPUT_MAX_CHARS {
        return s.to_string();
    }
    let start = s.len() - VERIFIER_INPUT_MAX_CHARS;
    let mut i = start;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    s[i..].to_string()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::match_wildcard_for_single_variants,
    clippy::manual_str_repeat,
    clippy::type_complexity,
    clippy::missing_const_for_fn,
    clippy::mutex_atomic
)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use origin_goal::verifier::{Verdict, Verifier, VerifierError};
    use origin_goal::{GoalState, TagOutcome};
    use std::sync::Mutex;

    /// Minimal scripted verifier. Pop replies front-to-back. Asserts on
    /// over-consumption so a bug that double-invokes the verifier blows up
    /// instead of silently passing.
    struct MockVerifier {
        replies: Mutex<Vec<Result<(Verdict, u64, u64), VerifierError>>>,
        call_count: std::sync::atomic::AtomicU32,
    }

    impl MockVerifier {
        fn new(replies: Vec<Result<(Verdict, u64, u64), VerifierError>>) -> Self {
            Self {
                replies: Mutex::new(replies),
                call_count: std::sync::atomic::AtomicU32::new(0),
            }
        }
        fn call_count(&self) -> u32 {
            self.call_count.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl Verifier for MockVerifier {
        async fn verify(
            &self,
            _condition: &str,
            _last_turn: &str,
        ) -> Result<(Verdict, u64, u64), VerifierError> {
            self.call_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut replies = self.replies.lock().unwrap();
            assert!(!replies.is_empty(), "mock verifier over-consumed");
            replies.remove(0)
        }
    }

    fn fresh(condition: &str) -> GoalState {
        GoalState::new(condition.into(), None, None)
    }

    #[tokio::test]
    async fn in_progress_returns_iterate_with_what_remains_in_prompt() {
        let mut g = fresh("fix tests");
        g.last_status_tag = Some(TagOutcome::InProgress {
            what_remains: "two tests still red".into(),
        });
        let v = MockVerifier::new(Vec::new());
        let d = drive(&mut g, "irrelevant", &v).await;
        match d {
            DriverDecision::Iterate {
                synthesized_prompt, ..
            } => {
                assert!(synthesized_prompt.contains("two tests still red"));
                assert!(synthesized_prompt.starts_with("[goal-driver]"));
            }
            other => panic!("expected Iterate, got {other:?}"),
        }
        assert_eq!(v.call_count(), 0, "verifier must not be called on in_progress");
    }

    #[tokio::test]
    async fn missing_returns_iterate_with_emit_tag_nudge() {
        let mut g = fresh("x");
        g.last_status_tag = Some(TagOutcome::Missing);
        let v = MockVerifier::new(Vec::new());
        let d = drive(&mut g, "", &v).await;
        match d {
            DriverDecision::Iterate {
                synthesized_prompt, ..
            } => {
                assert!(synthesized_prompt.contains("emit one this turn"));
            }
            other => panic!("expected Iterate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn blocked_returns_iterate_with_resolve_or_restate_nudge() {
        let mut g = fresh("x");
        g.last_status_tag = Some(TagOutcome::Blocked {
            why: "DB password unset".into(),
        });
        let v = MockVerifier::new(Vec::new());
        let d = drive(&mut g, "", &v).await;
        match d {
            DriverDecision::Iterate {
                synthesized_prompt, ..
            } => {
                assert!(synthesized_prompt.contains("DB password unset"));
                assert!(synthesized_prompt.contains("resolve the blocker"));
            }
            other => panic!("expected Iterate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn met_with_verifier_confirms_returns_cleared_met() {
        let mut g = fresh("x");
        g.last_status_tag = Some(TagOutcome::Met);
        let v = MockVerifier::new(vec![Ok((Verdict::Met, 500, 50))]);
        let d = drive(&mut g, "done!", &v).await;
        match d {
            DriverDecision::Cleared { reason, .. } => {
                assert!(matches!(reason, ClearReasonWire::Met { .. }));
            }
            other => panic!("expected Cleared, got {other:?}"),
        }
        assert_eq!(v.call_count(), 1);
        // Verifier tokens were charged to the goal's budget.
        assert_eq!(g.tokens_spent, 550);
    }

    #[tokio::test]
    async fn met_with_verifier_rejects_returns_iterate_with_reason() {
        let mut g = fresh("x");
        g.last_status_tag = Some(TagOutcome::Met);
        let v = MockVerifier::new(vec![Ok((
            Verdict::NotMet {
                reason: "tests still failing".into(),
            },
            400,
            40,
        ))]);
        let d = drive(&mut g, "done!", &v).await;
        match d {
            DriverDecision::Iterate {
                synthesized_prompt, ..
            } => {
                assert!(synthesized_prompt.contains("tests still failing"));
                assert!(synthesized_prompt.contains("verifier disagreed"));
            }
            other => panic!("expected Iterate, got {other:?}"),
        }
        assert_eq!(v.call_count(), 1);
        assert_eq!(g.tokens_spent, 440);
    }

    #[tokio::test]
    async fn met_with_verifier_rate_limited_returns_cleared_unavailable() {
        let mut g = fresh("x");
        g.last_status_tag = Some(TagOutcome::Met);
        let v = MockVerifier::new(vec![Err(VerifierError::RateLimit)]);
        let d = drive(&mut g, "done!", &v).await;
        match d {
            DriverDecision::Cleared { reason, .. } => {
                assert!(matches!(reason, ClearReasonWire::VerifierUnavailable));
            }
            other => panic!("expected Cleared, got {other:?}"),
        }
        assert_eq!(v.call_count(), 1);
    }

    #[tokio::test]
    async fn cap_check_max_iter_fires_without_calling_verifier_even_on_met() {
        // Set up a goal already at the iter cap; even with a Met tag the
        // cap check fires first and the verifier MUST NOT be invoked.
        let mut g = GoalState::new("x".into(), Some(3), None);
        g.iter = 3;
        g.last_status_tag = Some(TagOutcome::Met);
        let v = MockVerifier::new(Vec::new()); // empty — would panic if consumed
        let d = drive(&mut g, "done", &v).await;
        match d {
            DriverDecision::Cleared { reason, iter, .. } => {
                assert!(matches!(reason, ClearReasonWire::MaxIter));
                assert_eq!(iter, 3);
            }
            other => panic!("expected Cleared MaxIter, got {other:?}"),
        }
        assert_eq!(v.call_count(), 0);
    }

    #[tokio::test]
    async fn cap_check_budget_exhausted_fires_without_calling_verifier() {
        let mut g = GoalState::new("x".into(), None, Some(100));
        g.tokens_spent = 100;
        g.last_status_tag = Some(TagOutcome::Met);
        let v = MockVerifier::new(Vec::new());
        let d = drive(&mut g, "done", &v).await;
        match d {
            DriverDecision::Cleared {
                reason,
                tokens_spent,
                ..
            } => {
                assert!(matches!(reason, ClearReasonWire::BudgetExhausted));
                assert_eq!(tokens_spent, 100);
            }
            other => panic!("expected Cleared BudgetExhausted, got {other:?}"),
        }
        assert_eq!(v.call_count(), 0);
    }

    #[test]
    fn truncate_for_verifier_keeps_full_string_when_short() {
        let s = "short text";
        assert_eq!(truncate_for_verifier(s), s);
    }

    #[test]
    fn truncate_for_verifier_keeps_tail_when_over_limit() {
        let s: String = std::iter::repeat('a').take(VERIFIER_INPUT_MAX_CHARS + 100).collect();
        let out = truncate_for_verifier(&s);
        assert_eq!(out.len(), VERIFIER_INPUT_MAX_CHARS);
    }

    #[test]
    fn truncate_for_verifier_respects_utf8_boundary() {
        // 4-byte codepoint at the boundary — function must walk forward
        // until it lands on a codepoint start, so the result may be slightly
        // shorter than VERIFIER_INPUT_MAX_CHARS but must never split a UTF-8
        // sequence.
        let mut s = String::new();
        for _ in 0..VERIFIER_INPUT_MAX_CHARS - 2 {
            s.push('a');
        }
        s.push('\u{1F600}'); // 4 bytes; pushes past the limit
        for _ in 0..100 {
            s.push('z');
        }
        let out = truncate_for_verifier(&s);
        // Must be valid UTF-8 (i.e. not panic on .chars())
        assert!(out.chars().count() > 0);
    }
}
