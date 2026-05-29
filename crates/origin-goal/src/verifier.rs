// SPDX-License-Identifier: Apache-2.0
//! Verifier trait + plain-text verdict parser.
//!
//! The concrete Anthropic-Haiku implementation lives in the daemon to keep
//! this crate dependency-free. Tests use a `MockVerifier` defined inline.

use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Met,
    NotMet { reason: String },
}

#[allow(clippy::module_name_repetitions)] // `VerifierError` is the public error type
#[derive(Debug, Error)]
pub enum VerifierError {
    #[error("verifier transport: {0}")]
    Transport(String),
    #[error("verifier rate-limited")]
    RateLimit,
    #[error("verifier returned malformed output: {0}")]
    Malformed(String),
}

#[async_trait]
pub trait Verifier: Send + Sync {
    /// Run one verification. `condition` is the goal text; `last_turn` is the
    /// final assistant message (truncated by the caller to ≤4k chars).
    ///
    /// Returns `(Verdict, input_tokens, output_tokens)` so the driver can
    /// charge the verifier's spend against the goal's token budget.
    async fn verify(&self, condition: &str, last_turn: &str) -> Result<(Verdict, u64, u64), VerifierError>;
}

/// Parse a verdict from a raw Haiku response.
///
/// Expected format:
/// ```text
/// VERDICT: met
/// ```
/// or
/// ```text
/// VERDICT: not_met — tests still failing
/// ```
///
/// Tolerant of leading/trailing whitespace and `:` / `—` / `-` separators.
///
/// # Errors
/// Returns [`VerifierError::Malformed`] if no `VERDICT:` line is found or the
/// verdict word is neither `met` nor `not_met`.
pub fn parse_verdict(raw: &str) -> Result<Verdict, VerifierError> {
    for line in raw.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("VERDICT:") else {
            continue;
        };
        let rest = rest.trim();
        if let Some(reason) = rest.strip_prefix("not_met") {
            let reason = reason
                .trim_start_matches([' ', '\t', '-', '\u{2014}'])
                .trim()
                .to_string();
            return Ok(Verdict::NotMet { reason });
        }
        if rest == "met" || rest.starts_with("met ") || rest.starts_with("met\t") {
            return Ok(Verdict::Met);
        }
        return Err(VerifierError::Malformed(line.to_string()));
    }
    Err(VerifierError::Malformed(raw.to_string()))
}
