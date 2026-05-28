//! Shared harness for goal-driver integration tests (T12-T16).
//!
//! Cargo treats this `tests/goal_helpers/mod.rs` file as a helper module
//! (not a test binary) because it lives in a subdirectory of `tests/`.
//! Each test file pulls it in with `mod goal_helpers;`.
//!
//! Design constraints surfaced by Wave-3:
//! - The Anthropic verifier shares the same `Provider` as the main loop, so
//!   the scripted provider distinguishes verifier calls from main-loop calls
//!   by inspecting `ChatRequest::system`: the verifier's system prompt
//!   always starts with `"You verify whether a stated goal has been met"`.
//! - There is no `TestDaemon` — these helpers mimic the daemon's
//!   `drive_goal_loop` (see `crates/origin-daemon/src/main.rs`) so we test
//!   the driver logic without dragging in the connection plumbing.
//! - The peek-for-pending-user-frame mechanism in the real daemon requires a
//!   live IPC connection, so T15 stands in via a direct cap-check rather
//!   than the peek path.

#![allow(dead_code)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::missing_const_for_fn)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::goal_driver::{drive, DriverDecision};
use origin_daemon::protocol::StreamEvent;
use origin_goal::verifier::{Verdict, Verifier, VerifierError};
use origin_goal::{ClearReasonWire, GoalState, TagOutcome};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::Mutex;

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

/// Scripted `Verifier` impl wrapping the `ScriptedProvider`'s verifier
/// queue. Mirrors the production `AnthropicHaikuVerifier` enough to
/// exercise the driver: the parsed verdict comes back from the scripted
/// queue, and token counts are propagated to the goal state.
pub struct ScriptedVerifier<'p> {
    pub provider: &'p ScriptedProvider,
}

#[async_trait]
impl Verifier for ScriptedVerifier<'_> {
    async fn verify(
        &self,
        condition: &str,
        last_turn: &str,
    ) -> Result<(Verdict, u64, u64), VerifierError> {
        // Build the same shape the real Haiku verifier would, so the
        // ScriptedProvider's `is_verifier` check matches.
        let user_text = format!("Goal: {condition}\nAssistant's claim of completion: {last_turn}");
        let req = ChatRequest {
            system: format!("{VERIFIER_SYSTEM_PREFIX} based ONLY on the assistant's final response."),
            messages: vec![Message::new(Role::User).with_block(Block::text(user_text))],
            model: "claude-haiku-4-5".into(),
            tools: Vec::new(),
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
            StreamEvent::GoalCleared { reason, iter, tokens_spent } => {
                Some((reason, *iter, *tokens_spent))
            }
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

/// Run the driver loop against a scripted provider. Mimics
/// `drive_goal_loop` in `main.rs` minus the connection-peek plumbing
/// (which requires a live IPC connection).
///
/// `max_outer_iterations` is a paranoia cap so a buggy driver decision
/// can't hang the test. It does NOT enforce the goal's own `max_iter` —
/// the cap-check in `GoalState::cap_check` handles that.
pub async fn run_driver_loop_with_state(
    mut state: GoalState,
    initial_user_text: &str,
    provider: &ScriptedProvider,
    max_outer_iterations: u32,
) -> DriverRun {
    let verifier = ScriptedVerifier { provider };
    let mut run = DriverRun::default();
    let mut next_user_text = initial_user_text.to_string();

    for _ in 0..max_outer_iterations {
        // Top-of-iteration cap check (mirrors drive_goal_loop).
        if let Some(reason) = state.cap_check() {
            run.events.push(StreamEvent::GoalCleared {
                reason: reason.into(),
                iter: state.iter,
                tokens_spent: state.tokens_spent,
            });
            run.final_state = Some(state);
            return run;
        }

        // Build a minimal ChatRequest with `next_user_text` as the user
        // message so the captured-request log records the synthesized
        // driver prompts (T16 asserts on these). System prompt left empty
        // so the ScriptedProvider routes this to the main-loop queue.
        let req = ChatRequest {
            system: String::new(),
            messages: vec![
                Message::new(Role::User).with_block(Block::text(next_user_text.clone())),
            ],
            model: "claude-opus-4-7".into(),
            tools: Vec::new(),
        };
        let resp = match provider.chat(req).await {
            Ok(r) => r,
            Err(e) => panic!("scripted provider exhausted: {e}"),
        };

        run.iterations_run += 1;

        let assistant_text: String = resp
            .assistant
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        let tag = origin_goal::parse_tag(&assistant_text);
        state.record_iteration(
            u64::from(resp.usage.input_tokens),
            u64::from(resp.usage.output_tokens),
            tag.clone(),
        );

        // Emit `GoalVerifying` before calling the verifier so tests can
        // assert ordering, matching drive_goal_loop's behaviour.
        if matches!(tag, TagOutcome::Met) {
            run.events.push(StreamEvent::GoalVerifying);
        }

        let decision = drive(&mut state, &assistant_text, &verifier).await;
        match decision {
            DriverDecision::Iterate { iter_event, synthesized_prompt } => {
                run.events.push(iter_event);
                next_user_text = synthesized_prompt;
            }
            DriverDecision::Cleared { reason, iter, tokens_spent } => {
                run.events.push(StreamEvent::GoalCleared {
                    reason,
                    iter,
                    tokens_spent,
                });
                run.final_state = Some(state);
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
