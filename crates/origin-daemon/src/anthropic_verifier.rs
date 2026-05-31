// SPDX-License-Identifier: Apache-2.0
//! Concrete `Verifier` impl backed by a `Provider` (typically the
//! Anthropic Haiku model).
//!
//! Thin wrapper: builds a `ChatRequest` with the goal + last-turn pair,
//! dispatches via the provider, and parses the response's text blocks
//! through `origin_goal::verifier::parse_verdict`.
//!
//! Kept in the daemon (not in `origin-goal`) so the goal crate stays
//! dependency-free of `origin-provider`/`origin-core`.

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_goal::verifier::{parse_verdict, Verdict, Verifier, VerifierError};
use origin_provider::{ChatRequest, Provider};
use std::sync::Arc;

/// Verifier that asks a single `Provider::chat` call whether the goal is met.
///
/// `model` should be a small/cheap Haiku-class model — the verifier sees only
/// the goal text + the assistant's last turn (truncated to ≤4k chars by the
/// driver), so a low-latency model is the right choice.
pub struct AnthropicHaikuVerifier {
    pub provider: Arc<dyn Provider>,
    pub model: String,
}

const VERIFIER_SYSTEM: &str = "You verify whether a stated goal has been met based ONLY on \
                               the assistant's final response. Answer with exactly one of:\n\
                               VERDICT: met\n\
                               VERDICT: not_met — <one-sentence reason>";

#[async_trait]
impl Verifier for AnthropicHaikuVerifier {
    async fn verify(&self, condition: &str, last_turn: &str) -> Result<(Verdict, u64, u64), VerifierError> {
        let user_text = format!("Goal: {condition}\nAssistant's claim of completion: {last_turn}");
        let req = ChatRequest {
            system: VERIFIER_SYSTEM.to_string(),
            messages: vec![Message::new(Role::User).with_block(Block::text(user_text))],
            model: self.model.clone(),
            tools: Vec::new(),
            effort: None,
            thinking_tokens: None,
            attachments: Vec::new(),
        };
        let resp = self.provider.chat(req).await.map_err(|e| match e {
            origin_provider::ProviderError::RateLimit { .. } => VerifierError::RateLimit,
            other => VerifierError::Transport(other.to_string()),
        })?;
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
        // Bug #3: an empty assistant text would fall through to
        // `parse_verdict("")` which already returns `Malformed`, but the
        // error message is the empty string — useless when debugging a
        // verifier that's silently producing zero text blocks. Surface a
        // more informative error so logs make the failure mode obvious.
        if text.trim().is_empty() {
            return Err(VerifierError::Malformed("empty reply".into()));
        }
        let verdict = parse_verdict(&text)?;
        Ok((
            verdict,
            u64::from(resp.usage.input_tokens),
            u64::from(resp.usage.output_tokens),
        ))
    }
}
