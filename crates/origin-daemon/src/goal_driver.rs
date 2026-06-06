// SPDX-License-Identifier: Apache-2.0
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
use origin_goal::{
    state::MAX_CONSECUTIVE_VERIFIER_REJECTIONS, ClearReason, ClearReasonWire, GoalState, TagOutcome,
    TagOutcomeWire,
};

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

/// Owned inputs the driver needs to make a decision.
///
/// Caller copies the relevant fields out of `GoalState` under a short
/// lock, then calls [`drive_decision`] WITHOUT holding the slot lock.
/// The returned [`DecisionOutcome`] carries any mutations the caller
/// must apply to the state under a fresh lock. Bug #6.
#[derive(Debug)]
pub struct DriverInputs {
    pub condition: String,
    pub iter: u32,
    pub tokens_spent: u64,
    pub last_status_tag: Option<TagOutcome>,
    pub consecutive_rejections: u32,
}

impl DriverInputs {
    /// Snapshot the fields the driver needs out of a `GoalState`. Hold the
    /// slot lock only across this call, then drop it before calling
    /// [`drive_decision`].
    #[must_use]
    pub fn snapshot(state: &GoalState) -> Self {
        Self {
            condition: state.condition.clone(),
            iter: state.iter,
            tokens_spent: state.tokens_spent,
            last_status_tag: state.last_status_tag.clone(),
            consecutive_rejections: state.consecutive_rejections,
        }
    }
}

/// Mutations the caller must apply to the live `GoalState` after a driver call.
///
/// Built without the slot lock held; the caller re-acquires the lock,
/// applies these in order, then dispatches the [`DriverDecision`].
#[derive(Debug, Default)]
pub struct DecisionOutcome {
    pub decision: DriverDecision,
    /// Verifier input tokens to charge via `record_verifier_tokens`.
    pub verifier_in_tok: u64,
    /// Verifier output tokens to charge via `record_verifier_tokens`.
    pub verifier_out_tok: u64,
    /// Absolute value to assign to `state.consecutive_rejections`.
    /// `None` means "leave unchanged"; `Some(n)` means "set to n".
    pub set_consecutive_rejections: Option<u32>,
}

impl Default for DriverDecision {
    fn default() -> Self {
        Self::Cleared {
            reason: ClearReasonWire::VerifierUnavailable,
            iter: 0,
            tokens_spent: 0,
        }
    }
}

/// Pure-async driver dispatch.
///
/// Takes owned inputs (no slot lock held across the await) and returns
/// mutations + decision for the caller to apply. Bug #6.
#[allow(clippy::too_many_lines)] // the match-on-tag dispatch is one logical unit
pub async fn drive_decision(
    inputs: DriverInputs,
    last_turn_text: &str,
    verifier: &dyn Verifier,
) -> DecisionOutcome {
    let tag = inputs.last_status_tag.clone().unwrap_or(TagOutcome::Missing);
    match tag {
        TagOutcome::Met => {
            let truncated = truncate_for_verifier(last_turn_text);
            match verifier.verify(&inputs.condition, &truncated).await {
                Ok((Verdict::Met, in_tok, out_tok)) => DecisionOutcome {
                    // The caller will charge tokens THEN cap-check before
                    // committing to `Met` (Bug #11). The decision here is
                    // tentative; we still pass `Met` so the caller knows
                    // the verifier confirmed.
                    decision: cleared_from_inputs(
                        &inputs,
                        in_tok + out_tok,
                        ClearReason::Met {
                            reason: "verifier confirmed".into(),
                        },
                    ),
                    verifier_in_tok: in_tok,
                    verifier_out_tok: out_tok,
                    // Bug #15: reset the rejection counter on a successful met.
                    set_consecutive_rejections: Some(0),
                },
                Ok((Verdict::NotMet { reason }, in_tok, out_tok)) => {
                    // Bug #15: count consecutive rejections; clear with
                    // `VerifierRejected` once the cap is hit.
                    let next = inputs.consecutive_rejections.saturating_add(1);
                    if next >= MAX_CONSECUTIVE_VERIFIER_REJECTIONS {
                        DecisionOutcome {
                            decision: cleared_from_inputs(
                                &inputs,
                                in_tok + out_tok,
                                ClearReason::VerifierRejected(reason),
                            ),
                            verifier_in_tok: in_tok,
                            verifier_out_tok: out_tok,
                            set_consecutive_rejections: Some(next),
                        }
                    } else {
                        DecisionOutcome {
                            decision: iterate_from_inputs(
                                &inputs,
                                in_tok + out_tok,
                                format!(
                                    "[goal-driver] You claimed the goal was met, but the verifier disagreed: \
                                     {reason}. Address that specific gap and continue."
                                ),
                            ),
                            verifier_in_tok: in_tok,
                            verifier_out_tok: out_tok,
                            set_consecutive_rejections: Some(next),
                        }
                    }
                }
                Err(VerifierError::Malformed(_)) => {
                    // Bug #3: malformed verifier output is NOT fail-open. Treat
                    // it as `NotMet { unparseable }` so the iteration resumes
                    // and the model gets another chance instead of falsely
                    // confirming the goal.
                    let next = inputs.consecutive_rejections.saturating_add(1);
                    if next >= MAX_CONSECUTIVE_VERIFIER_REJECTIONS {
                        DecisionOutcome {
                            decision: cleared_from_inputs(
                                &inputs,
                                0,
                                ClearReason::VerifierRejected("verifier returned unparseable output".into()),
                            ),
                            set_consecutive_rejections: Some(next),
                            ..DecisionOutcome::default()
                        }
                    } else {
                        DecisionOutcome {
                            decision: iterate_from_inputs(
                                &inputs,
                                0,
                                "[goal-driver] The verifier returned unparseable output; \
                                 re-state your status this turn and emit a fresh \
                                 <goal-status> tag."
                                    .to_string(),
                            ),
                            set_consecutive_rejections: Some(next),
                            ..DecisionOutcome::default()
                        }
                    }
                }
                Err(VerifierError::RateLimit | VerifierError::Transport(_)) => {
                    // Fail open — trust the main model's `met` claim rather
                    // than burn more budget on a verifier we can't reach.
                    DecisionOutcome {
                        decision: cleared_from_inputs(&inputs, 0, ClearReason::VerifierUnavailable),
                        ..DecisionOutcome::default()
                    }
                }
            }
        }
        TagOutcome::InProgress { what_remains } => DecisionOutcome {
            decision: iterate_from_inputs(
                &inputs,
                0,
                format!(
                    "[goal-driver] Continue toward the active goal. What remains: {}",
                    if what_remains.is_empty() {
                        "unspecified — keep going.".to_string()
                    } else {
                        what_remains
                    }
                ),
            ),
            // Bug #15: any non-Met tag resets the rejection counter.
            set_consecutive_rejections: Some(0),
            ..DecisionOutcome::default()
        },
        TagOutcome::Missing => DecisionOutcome {
            decision: iterate_from_inputs(
                &inputs,
                0,
                "[goal-driver] Continue toward the active goal. What remains: \
                 unknown — main model did not emit a <goal-status> tag last turn; \
                 emit one this turn."
                    .to_string(),
            ),
            set_consecutive_rejections: Some(0),
            ..DecisionOutcome::default()
        },
        TagOutcome::Blocked { why } => DecisionOutcome {
            // The model reported the goal blocked on something requiring the
            // human. Clear the goal now so the loop stops and the user can
            // respond to the restated blocker. Iterating instead (the previous
            // behavior) just re-injects the same guidance every turn and spins
            // until the max-iteration cap eventually trips — burning tokens on a
            // block the driver cannot resolve autonomously.
            decision: cleared_from_inputs(&inputs, 0, ClearReason::Blocked { why }),
            set_consecutive_rejections: Some(0),
            ..DecisionOutcome::default()
        },
    }
}

/// Legacy facade — mutates `state` under the caller's existing lock contract.
///
/// New callers should use [`drive_decision`] + [`apply_outcome`] for the
/// lock-free async path; this wrapper preserves the original `drive` API
/// for tests and any callers that don't need the lock optimization.
///
/// Bug #16: the legacy duplicate `cap_check` was removed; the caller is
/// now expected to perform the cap-check before invoking `drive`. The
/// unit tests of `drive` were updated to match.
///
/// Bug #11: after the verifier returns `Met`, the caller's `cap_check`
/// fires once more — handled in `apply_outcome`.
pub async fn drive(state: &mut GoalState, last_turn_text: &str, verifier: &dyn Verifier) -> DriverDecision {
    let inputs = DriverInputs::snapshot(state);
    let outcome = drive_decision(inputs, last_turn_text, verifier).await;
    apply_outcome(state, outcome)
}

/// Apply the driver's mutations to `state` and return the final [`DriverDecision`].
///
/// Implements Bug #11: when the verifier confirmed `Met` but the
/// post-charge `cap_check` fires (budget exhausted by the verifier's own
/// spend), the cap reason wins.
#[must_use]
pub fn apply_outcome(state: &mut GoalState, outcome: DecisionOutcome) -> DriverDecision {
    state.record_verifier_tokens(outcome.verifier_in_tok, outcome.verifier_out_tok);
    if let Some(n) = outcome.set_consecutive_rejections {
        state.consecutive_rejections = n;
    }
    // Bug #11: after charging verifier spend, re-check the budget cap.
    // If the verifier pushed us past budget, prefer the cap reason over
    // any `Met` the driver tentatively decided on.
    if matches!(outcome.decision, DriverDecision::Cleared { ref reason, .. }
        if matches!(reason, ClearReasonWire::Met { .. }))
    {
        if let Some(cap_reason) = state.cap_check() {
            return DriverDecision::Cleared {
                reason: cap_reason.into(),
                iter: state.iter,
                tokens_spent: state.tokens_spent,
            };
        }
    }
    // Refresh iter/tokens_spent in the decision so the wire event reflects
    // the post-charge totals (the original decision was built from the
    // pre-charge snapshot).
    match outcome.decision {
        DriverDecision::Cleared { reason, .. } => DriverDecision::Cleared {
            reason,
            iter: state.iter,
            tokens_spent: state.tokens_spent,
        },
        DriverDecision::Iterate {
            synthesized_prompt,
            iter_event: _,
        } => DriverDecision::Iterate {
            synthesized_prompt,
            iter_event: StreamEvent::GoalIteration {
                iter: state.iter,
                tokens_spent: state.tokens_spent,
                last_tag: TagOutcomeWire::from(state.last_status_tag.clone().unwrap_or(TagOutcome::Missing)),
            },
        },
    }
}

fn cleared_from_inputs(inputs: &DriverInputs, extra_tokens: u64, reason: ClearReason) -> DriverDecision {
    DriverDecision::Cleared {
        reason: reason.into(),
        iter: inputs.iter,
        tokens_spent: inputs.tokens_spent.saturating_add(extra_tokens),
    }
}

fn iterate_from_inputs(inputs: &DriverInputs, extra_tokens: u64, prompt: String) -> DriverDecision {
    let iter_event = StreamEvent::GoalIteration {
        iter: inputs.iter,
        tokens_spent: inputs.tokens_spent.saturating_add(extra_tokens),
        last_tag: TagOutcomeWire::from(inputs.last_status_tag.clone().unwrap_or(TagOutcome::Missing)),
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
            self.call_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
    async fn blocked_clears_goal_with_blocked_reason() {
        // A blocked goal requires the human, so the driver clears it (surfacing
        // the blocker) rather than iterating — iterating would just spin,
        // re-injecting the same nudge every turn until the max-iter cap trips.
        let mut g = fresh("x");
        g.last_status_tag = Some(TagOutcome::Blocked {
            why: "DB password unset".into(),
        });
        let v = MockVerifier::new(Vec::new());
        let d = drive(&mut g, "", &v).await;
        match d {
            DriverDecision::Cleared { reason, .. } => match reason {
                ClearReasonWire::Blocked { why } => assert_eq!(why, "DB password unset"),
                other => panic!("expected Blocked reason, got {other:?}"),
            },
            other => panic!("expected Cleared, got {other:?}"),
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

    // Bug #16: `drive` no longer does its own cap_check; the caller is
    // expected to check `state.cap_check()` BEFORE calling `drive`. The
    // tests below verify that contract: when `drive` is called on a state
    // already past the cap, it still dispatches via the tag (it does NOT
    // emit a phantom `MaxIter` reason). The cap_check is the caller's job.
    #[tokio::test]
    async fn drive_dispatches_by_tag_even_when_caller_skipped_cap_check() {
        // This documents the new contract — `drive` will happily call the
        // verifier even when iter > max_iter, because the cap_check is the
        // caller's responsibility (deduplicated from `main.rs::1576`).
        //
        // Note: we use a HIGH iter cap here so the post-charge cap_check
        // (Bug #11) does NOT fire — the point of this test is to prove
        // that `drive` no longer short-circuits on cap_check at the top.
        let mut g = GoalState::new("x".into(), Some(100), None);
        g.iter = 50;
        g.last_status_tag = Some(TagOutcome::Met);
        let v = MockVerifier::new(vec![Ok((Verdict::Met, 10, 5))]);
        let d = drive(&mut g, "done", &v).await;
        // Verifier WAS called; the cap is the caller's responsibility now.
        assert_eq!(v.call_count(), 1);
        // The verifier confirmed Met, so the decision is Cleared { Met }.
        match d {
            DriverDecision::Cleared { reason, .. } => {
                assert!(matches!(reason, ClearReasonWire::Met { .. }));
            }
            other => panic!("expected Cleared Met, got {other:?}"),
        }
    }

    // Bug #11: when the verifier returns Met but its own token spend pushes
    // us past the budget, the cap reason wins over Met.
    #[tokio::test]
    async fn verifier_met_but_post_charge_budget_exhausted_clears_as_budget() {
        // Budget = 100; pre-call we've already spent 50; verifier returns
        // Met with 60 token spend → total 110 > 100. Cap fires.
        let mut g = GoalState::new("x".into(), None, Some(100));
        g.tokens_spent = 50;
        g.last_status_tag = Some(TagOutcome::Met);
        let v = MockVerifier::new(vec![Ok((Verdict::Met, 40, 20))]);
        let d = drive(&mut g, "done", &v).await;
        match d {
            DriverDecision::Cleared {
                reason, tokens_spent, ..
            } => {
                assert!(
                    matches!(reason, ClearReasonWire::BudgetExhausted),
                    "expected BudgetExhausted, got {reason:?}"
                );
                assert_eq!(tokens_spent, 110);
            }
            other => panic!("expected Cleared BudgetExhausted, got {other:?}"),
        }
    }

    // Bug #3: malformed verifier output must NOT fail-open as Met. It
    // should retry (treated as NotMet { unparseable }) so the model gets
    // another chance.
    #[tokio::test]
    async fn malformed_verifier_output_retries_does_not_clear_as_met() {
        let mut g = fresh("x");
        g.last_status_tag = Some(TagOutcome::Met);
        let v = MockVerifier::new(vec![Err(VerifierError::Malformed("garbage".into()))]);
        let d = drive(&mut g, "done", &v).await;
        match d {
            DriverDecision::Iterate {
                synthesized_prompt, ..
            } => {
                assert!(
                    synthesized_prompt.contains("unparseable"),
                    "expected the synthesized prompt to mention unparseable; got: {synthesized_prompt:?}"
                );
            }
            DriverDecision::Cleared { reason, .. } => {
                panic!("malformed verifier output must NOT clear as Met or Unavailable; got {reason:?}");
            }
        }
        assert_eq!(v.call_count(), 1);
    }

    // Bug #3 control: a RateLimit (vs Malformed) still fails OPEN — that's
    // intentional, so the model doesn't get stuck waiting on a verifier
    // we genuinely can't reach.
    #[tokio::test]
    async fn rate_limit_still_fails_open_as_verifier_unavailable() {
        let mut g = fresh("x");
        g.last_status_tag = Some(TagOutcome::Met);
        let v = MockVerifier::new(vec![Err(VerifierError::RateLimit)]);
        let d = drive(&mut g, "done", &v).await;
        match d {
            DriverDecision::Cleared { reason, .. } => {
                assert!(matches!(reason, ClearReasonWire::VerifierUnavailable));
            }
            other => panic!("expected Cleared VerifierUnavailable, got {other:?}"),
        }
    }

    // Bug #15: after MAX_CONSECUTIVE_VERIFIER_REJECTIONS NotMet returns,
    // the driver gives up and emits Cleared { VerifierRejected }.
    #[tokio::test]
    async fn three_consecutive_rejections_clear_as_verifier_rejected() {
        let mut g = fresh("x");
        // Simulate the state after two prior rejections — the third should
        // trip the cap.
        g.consecutive_rejections = 2;
        g.last_status_tag = Some(TagOutcome::Met);
        let v = MockVerifier::new(vec![Ok((
            Verdict::NotMet {
                reason: "still failing".into(),
            },
            10,
            5,
        ))]);
        let d = drive(&mut g, "done", &v).await;
        match d {
            DriverDecision::Cleared { reason, .. } => match reason {
                ClearReasonWire::VerifierRejected { why } => {
                    assert!(why.contains("still failing"));
                }
                other => panic!("expected VerifierRejected, got {other:?}"),
            },
            other => panic!("expected Cleared, got {other:?}"),
        }
        assert_eq!(g.consecutive_rejections, 3);
    }

    // Bug #15: a non-Met tag (or a Met that the verifier confirms) resets
    // the counter so a stale prior-rejection streak doesn't get carried.
    #[tokio::test]
    async fn in_progress_tag_resets_consecutive_rejections() {
        let mut g = fresh("x");
        g.consecutive_rejections = 2;
        g.last_status_tag = Some(TagOutcome::InProgress {
            what_remains: "more work".into(),
        });
        let v = MockVerifier::new(Vec::new());
        let _ = drive(&mut g, "", &v).await;
        assert_eq!(g.consecutive_rejections, 0);
    }

    #[tokio::test]
    async fn verifier_met_resets_consecutive_rejections() {
        let mut g = fresh("x");
        g.consecutive_rejections = 2;
        g.last_status_tag = Some(TagOutcome::Met);
        let v = MockVerifier::new(vec![Ok((Verdict::Met, 5, 5))]);
        let _ = drive(&mut g, "done", &v).await;
        assert_eq!(g.consecutive_rejections, 0);
    }

    #[test]
    fn truncate_for_verifier_keeps_full_string_when_short() {
        let s = "short text";
        assert_eq!(truncate_for_verifier(s), s);
    }

    #[test]
    fn truncate_for_verifier_keeps_tail_when_over_limit() {
        let s: String = std::iter::repeat_n('a', VERIFIER_INPUT_MAX_CHARS + 100).collect();
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
