// SPDX-License-Identifier: Apache-2.0
//! Shared harness for goal-driver integration tests (T12-T16, plus the
//! Phase-9 follow-up suite added on top).
//!
//! Cargo treats this `tests/goal_helpers/mod.rs` file as a helper module
//! (not a test binary) because it lives in a subdirectory of `tests/`.
//! Each test file pulls it in with `mod goal_helpers;`.
//!
//! ## What's real, what's still a mimic
//!
//! The Wave-4 version of this harness short-circuited `run_loop` entirely
//! and called `goal_driver::drive` against the scripted provider directly.
//! That made the tests fast but bypassed every system-prompt-assembly path
//! in `agent.rs`, including the `<origin-goal>` block render — exactly the
//! place a regression would land.
//!
//! Wave-9 follow-up: the harness now drives REAL `run_loop` for every
//! iteration via `LoopOptions { streaming_disabled: true, .. }`, with a
//! shared `Arc<Mutex<Option<GoalState>>>` slot mounted into
//! `LoopOptions::goal` so the assembled system prompt actually contains
//! the `<origin-goal>` block the production driver assembles. The outer
//! decision loop still mimics `drive_goal_loop` in `main.rs` (peek + push-
//! back + checkpoint writes need a live `SharedConnection` and a
//! `SessionStore`), but the per-iteration provider request now flows
//! through the real `run_loop` + system-prompt assembly path.
//!
//! Distinguishing verifier calls from main calls: the Anthropic verifier
//! shares the same `Provider`. The scripted provider routes by inspecting
//! `ChatRequest::system` — the verifier's system prompt always starts
//! with `"You verify whether a stated goal has been met"`. The main
//! loop's system prompt is now non-trivial (identity block, directives,
//! optionally the `<origin-goal>` block) so the prefix discrimination
//! still holds.

#![allow(dead_code)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::redundant_clone)]
#![allow(clippy::needless_collect)]
#![allow(clippy::unnecessary_join)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::significant_drop_tightening)]
#![allow(clippy::match_like_matches_macro)]
#![allow(clippy::equatable_if_let)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions, LoopSummary};
use origin_daemon::goal_driver::{drive, DriverDecision};
use origin_daemon::protocol::{ClientMessage, StreamEvent};
use origin_daemon::session::Session;
use origin_goal::verifier::{Verdict, Verifier, VerifierError};
use origin_goal::{ClearReasonWire, GoalState, TagOutcome};
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::{Arc, Mutex};

/// Prefix that uniquely identifies a verifier `chat` call so the scripted
/// provider can route it to the verifier-reply queue. Matches the constant
/// inside `crates/origin-daemon/src/anthropic_verifier.rs`.
pub const VERIFIER_SYSTEM_PREFIX: &str = "You verify whether a stated goal has been met";

/// Captured copy of the `ChatRequest` the harness saw, broken into a
/// reduced shape so tests can assert on the fields they care about.
#[derive(Debug, Clone)]
pub struct CapturedRequest {
    pub is_verifier: bool,
    /// Full text of the last user message in the request, if any.
    pub last_user_text: String,
    /// Full system prompt the request carried. The Wave-4 mimic always set
    /// this to `""` on the main-loop side; the Wave-9 real-`run_loop`
    /// path populates it with the assembled identity + directive +
    /// `<origin-goal>` block payload, which is what
    /// `goal_system_prompt_injection.rs` asserts on.
    pub system: String,
}

/// Two-queue scripted provider: separates main-loop replies from
/// verifier-loop replies so a single shared `Provider` can serve both.
pub struct ScriptedProvider {
    main: Mutex<Vec<ChatResponse>>,
    verifier: Mutex<Vec<ChatResponse>>,
    captured: Mutex<Vec<CapturedRequest>>,
}

impl ScriptedProvider {
    pub fn new() -> Self {
        Self {
            main: Mutex::new(Vec::new()),
            verifier: Mutex::new(Vec::new()),
            captured: Mutex::new(Vec::new()),
        }
    }

    /// Push a canned main-loop assistant text. Usage defaults to zero.
    pub fn with_main_reply(self, text: &str) -> Self {
        self.with_main_reply_usage(text, 0, 0)
    }

    /// Push a canned main-loop assistant text plus a usage payload so tests
    /// can drive the budget cap with deterministic token counts.
    pub fn with_main_reply_usage(self, text: &str, input_tokens: u32, output_tokens: u32) -> Self {
        self.main.lock().unwrap().push(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text(text)),
            usage: Usage {
                input_tokens,
                output_tokens,
                ..Usage::default()
            },
        });
        self
    }

    /// Push a canned verifier reply. Text should follow the `VERDICT:` line
    /// format the verdict-parser expects (see `origin-goal::verifier`).
    pub fn with_verifier_reply(self, text: &str) -> Self {
        self.verifier.lock().unwrap().push(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text(text)),
            usage: Usage::default(),
        });
        self
    }

    pub fn captured(&self) -> Vec<CapturedRequest> {
        self.captured.lock().unwrap().clone()
    }

    pub fn main_remaining(&self) -> usize {
        self.main.lock().unwrap().len()
    }

    pub fn verifier_remaining(&self) -> usize {
        self.verifier.lock().unwrap().len()
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn name(&self) -> &'static str {
        "scripted-goal"
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let is_verifier = req.system.starts_with(VERIFIER_SYSTEM_PREFIX);
        let last_user_text = last_user_message_text(&req.messages);
        self.captured.lock().unwrap().push(CapturedRequest {
            is_verifier,
            last_user_text,
            system: req.system.clone(),
        });
        let mut q = if is_verifier {
            self.verifier.lock().unwrap()
        } else {
            self.main.lock().unwrap()
        };
        if q.is_empty() {
            return Err(ProviderError::Api(format!(
                "scripted provider exhausted ({} queue)",
                if is_verifier { "verifier" } else { "main" }
            )));
        }
        Ok(q.remove(0))
    }
}

fn last_user_message_text(messages: &[Message]) -> String {
    for msg in messages.iter().rev() {
        if matches!(msg.role, Role::User) {
            return msg
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
        }
    }
    String::new()
}

fn last_assistant_text(session: &Session) -> String {
    for msg in session.messages.iter().rev() {
        if matches!(msg.role, Role::Assistant) {
            return msg
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
        }
    }
    String::new()
}

/// Scripted `Verifier` impl wrapping the `ScriptedProvider`'s verifier
/// queue. Mirrors the production `AnthropicHaikuVerifier` enough to
/// exercise the driver: the parsed verdict comes back from the scripted
/// queue, and token counts are propagated to the goal state.
pub struct ScriptedVerifier<'p> {
    pub provider: &'p ScriptedProvider,
}

#[async_trait]
impl Verifier for ScriptedVerifier<'_> {
    async fn verify(&self, condition: &str, last_turn: &str) -> Result<(Verdict, u64, u64), VerifierError> {
        // Build the same shape the real Haiku verifier would, so the
        // ScriptedProvider's `is_verifier` check matches.
        let user_text = format!("Goal: {condition}\nAssistant's claim of completion: {last_turn}");
        let req = ChatRequest {
            system: format!("{VERIFIER_SYSTEM_PREFIX} based ONLY on the assistant's final response."),
            messages: vec![Message::new(Role::User).with_block(Block::text(user_text))],
            model: "claude-haiku-4-5".into(),
            tools: Vec::new(),
            effort: None,
            attachments: Vec::new(),
        };
        let resp = self
            .provider
            .chat(req)
            .await
            .map_err(|e| VerifierError::Transport(e.to_string()))?;
        let text: String = resp
            .assistant
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let verdict = origin_goal::verifier::parse_verdict(&text)?;
        Ok((
            verdict,
            u64::from(resp.usage.input_tokens),
            u64::from(resp.usage.output_tokens),
        ))
    }
}

/// Optional hook the test can register to manipulate the harness BETWEEN
/// iterations — e.g. drop a `ClientMessage::Interrupt` into the
/// `pending_message` slot so the next outer-loop tick sees it. The hook is
/// invoked after `goal_driver::drive` returns `Iterate` but BEFORE the
/// harness drains the push-back buffer, matching the order of operations
/// in `drive_goal_loop`'s peek-between-iterations branch.
pub type BetweenIterHook = Box<
    dyn for<'a> Fn(
            u32,
            &'a Arc<tokio::sync::Mutex<Option<ClientMessage>>>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
        + Send
        + Sync,
>;

/// Output of one driver-loop run. Tests can pattern-match on the trailing
/// event (always a `GoalCleared` once the loop terminates) and inspect the
/// iteration timeline.
#[derive(Debug, Default)]
pub struct DriverRun {
    pub events: Vec<StreamEvent>,
    pub iterations_run: u32,
    pub final_state: Option<GoalState>,
}

impl DriverRun {
    pub fn last_event(&self) -> Option<&StreamEvent> {
        self.events.last()
    }

    pub fn cleared(&self) -> Option<(&ClearReasonWire, u32, u64)> {
        match self.events.last()? {
            StreamEvent::GoalCleared {
                reason,
                iter,
                tokens_spent,
            } => Some((reason, *iter, *tokens_spent)),
            _ => None,
        }
    }

    pub fn iteration_count(&self) -> usize {
        self.events
            .iter()
            .filter(|e| matches!(e, StreamEvent::GoalIteration { .. }))
            .count()
    }
}

/// Convenience wrapper around [`run_driver_loop_full`] that preserves the
/// pre-Wave-9 call shape used by the existing test files.
pub async fn run_driver_loop_with_state(
    state: GoalState,
    initial_user_text: &str,
    provider: &ScriptedProvider,
    max_outer_iterations: u32,
) -> DriverRun {
    run_driver_loop_full(state, initial_user_text, provider, max_outer_iterations, None).await
}

/// Run the driver loop against a scripted provider, exercising the REAL
/// `run_loop` for each iteration. The outer decision loop still mimics
/// `drive_goal_loop` (in `main.rs`) — the production driver's
/// peek-between-iterations branch needs a live `SharedConnection`, which
/// no integration test can provide. The mimic now keeps a
/// `pending_message: Arc<Mutex<Option<ClientMessage>>>` slot so tests can
/// validate the Interrupt push-back contract end-to-end.
///
/// `max_outer_iterations` is a paranoia cap so a buggy driver decision
/// can't hang the test. It does NOT enforce the goal's own `max_iter` —
/// the cap-check in `GoalState::cap_check` handles that.
pub async fn run_driver_loop_full(
    state: GoalState,
    initial_user_text: &str,
    provider: &ScriptedProvider,
    max_outer_iterations: u32,
    between_iter_hook: Option<BetweenIterHook>,
) -> DriverRun {
    let verifier = ScriptedVerifier { provider };
    let mut run = DriverRun::default();
    let mut next_user_text = initial_user_text.to_string();

    // Per-connection state shared between this harness and `run_loop`. The
    // goal slot is the same `Arc<Mutex<Option<GoalState>>>` shape main.rs
    // wires through `LoopOptions::with_goal`, so the system-prompt assembly
    // in `agent.rs` will render the `<origin-goal>` block on every turn.
    let goal_slot: Arc<tokio::sync::Mutex<Option<GoalState>>> =
        Arc::new(tokio::sync::Mutex::new(Some(state)));
    let pending_message: Arc<tokio::sync::Mutex<Option<ClientMessage>>> =
        Arc::new(tokio::sync::Mutex::new(None));

    // Build the LoopOptions once. `streaming_disabled = true` routes
    // through `provider.chat()` — the deterministic, non-streaming path —
    // which is what tests want and what the existing `loop_e2e.rs` pattern
    // uses for scripted providers.
    let opts = LoopOptions {
        streaming_disabled: true,
        goal: Arc::clone(&goal_slot),
        ..LoopOptions::default()
    };

    // Session is rebuilt fresh — the daemon's per-prompt handler also gets
    // a Session loaded from storage, but for these tests a clean in-memory
    // Session is sufficient: each `run_loop` call appends to it, the next
    // call sees the prior history, and the goal block in the system prompt
    // is what the test asserts on.
    let mut session = Session::new("test", "claude-opus-4-7");

    for _ in 0..max_outer_iterations {
        // Top-of-iteration cap check (mirrors drive_goal_loop). We do this
        // BEFORE the run_loop call so a buggy driver can't blow the cap by
        // one extra provider call.
        {
            let mut slot = goal_slot.lock().await;
            if let Some(g) = slot.as_mut() {
                if let Some(reason) = g.cap_check() {
                    let iter = g.iter;
                    let tokens_spent = g.tokens_spent;
                    let final_state = slot.take();
                    drop(slot);
                    run.events.push(StreamEvent::GoalCleared {
                        reason: reason.into(),
                        iter,
                        tokens_spent,
                    });
                    run.final_state = final_state;
                    return run;
                }
            }
        }

        // Drain the push-back buffer BEFORE the run_loop call. The
        // production driver's policy is "Interrupt clears the goal and is
        // consumed; other variants clear the goal and are pushed back for
        // the outer loop to dispatch". The mimic handles Interrupt the
        // same way: if a peeked Interrupt is sitting in the slot, clear
        // the goal and stop iterating (the outer loop in main.rs would
        // then re-enter on the next user prompt; for test purposes the
        // run is over).
        {
            let buffered = pending_message.lock().await.take();
            if let Some(ClientMessage::Interrupt) = buffered {
                let mut slot = goal_slot.lock().await;
                if let Some(prior) = slot.take() {
                    run.events.push(StreamEvent::GoalCleared {
                        reason: ClearReasonWire::UserSlash,
                        iter: prior.iter,
                        tokens_spent: prior.tokens_spent,
                    });
                    run.final_state = Some(prior);
                }
                return run;
            }
            // Non-interrupt variants: the production driver pushes those
            // back through `pending_message` for the outer message loop;
            // the harness has no outer loop, so we just discard them
            // after firing the same GoalCleared{UserSlash} event the
            // production driver would. Tests that need to assert on the
            // pushed-back body should inspect `pending_message` directly
            // via the hook.
            if buffered.is_some() {
                let mut slot = goal_slot.lock().await;
                if let Some(prior) = slot.take() {
                    run.events.push(StreamEvent::GoalCleared {
                        reason: ClearReasonWire::UserSlash,
                        iter: prior.iter,
                        tokens_spent: prior.tokens_spent,
                    });
                    run.final_state = Some(prior);
                }
                return run;
            }
        }

        // === REAL `run_loop` CALL ===
        // Streaming disabled, goal slot mounted, AlwaysAllow prompter.
        // Errors here are surfaced verbatim so a test sees the actual
        // ProviderError text (the ScriptedProvider's "exhausted" error
        // text is what the Wave-4 panic was wrapping).
        let summary: LoopSummary =
            match run_loop(&mut session, &next_user_text, provider, &AlwaysAllow, &opts).await {
                Ok(s) => s,
                Err(e) => panic!("run_loop failed: {e}"),
            };

        run.iterations_run += 1;

        // Pull the assistant text run_loop appended to the session, parse
        // the <goal-status> tag, record the iteration's spend + tag, then
        // call the same `drive` the production code calls.
        let assistant_text = last_assistant_text(&session);
        let tag = origin_goal::parse_tag(&assistant_text);

        // Drive + decide under the goal lock. Mirror drive_goal_loop's
        // ordering: record_iteration first, emit GoalVerifying if Met,
        // then call drive (which will invoke the verifier on Met).
        let decision = {
            let mut slot = goal_slot.lock().await;
            let Some(g) = slot.as_mut() else {
                // Goal cleared by another path — return what we have. The
                // mimic doesn't have an outer-loop fallback, so this is
                // an early-exit identical to the production case.
                run.final_state = None;
                return run;
            };
            g.record_iteration(summary.input_tokens, summary.output_tokens, tag);
            if matches!(g.last_status_tag, Some(TagOutcome::Met)) {
                run.events.push(StreamEvent::GoalVerifying);
            }
            drive(g, &assistant_text, &verifier).await
        };

        match decision {
            DriverDecision::Iterate {
                iter_event,
                synthesized_prompt,
            } => {
                run.events.push(iter_event);
                next_user_text = synthesized_prompt;

                // Give the test a chance to inject a peeked message
                // between iterations — this is the harness analogue of
                // drive_goal_loop's mid-iteration peek branch.
                if let Some(hook) = between_iter_hook.as_ref() {
                    let iter_now = {
                        let guard = goal_slot.lock().await;
                        guard.as_ref().map_or(0, |g| g.iter)
                    };
                    hook(iter_now, &pending_message).await;
                }
            }
            DriverDecision::Cleared {
                reason,
                iter,
                tokens_spent,
            } => {
                let final_state = goal_slot.lock().await.take();
                run.events.push(StreamEvent::GoalCleared {
                    reason,
                    iter,
                    tokens_spent,
                });
                run.final_state = final_state;
                return run;
            }
        }
    }

    panic!(
        "driver loop did not terminate within {max_outer_iterations} outer iterations \
         (likely a buggy decision); events so far: {:?}",
        run.events
    );
}
