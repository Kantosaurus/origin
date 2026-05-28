//! Token-aware result builder. Every tool produces its serialised result
//! through `ResultWriter`, which enforces a per-call token budget and emits
//! a structured continuation handle on overflow.

use serde_json::{json, Value};

#[must_use]
pub fn approx_tokens(s: &str) -> usize {
    // chars/4 with a small punctuation-density correction. We accept ±10% drift
    // — exact tokenisation per call would dominate envelope latency.
    let chars = s.chars().count();
    chars / 4
}

/// Builder for a tool result, capped at `budget_tokens` approximate tokens.
///
/// Callers `push_str` body fragments and (optionally) `note_line(idx)` after
/// each logical record so that on overflow the continuation handle can resume
/// from the right place.
pub struct ResultWriter {
    budget_tokens: u32,
    used_tokens: u32,
    body: String,
    tool_name: String,
    base_args: Value,
    last_line_noted: Option<u32>,
    overflowed: bool,
}

impl ResultWriter {
    #[must_use]
    pub fn new(budget_tokens: u32, tool_name: impl Into<String>, base_args: Value) -> Self {
        Self {
            budget_tokens,
            used_tokens: 0,
            body: String::new(),
            tool_name: tool_name.into(),
            base_args,
            last_line_noted: None,
            overflowed: false,
        }
    }

    /// Returns the number of tokens consumed so far (per the impl's approximation).
    #[must_use]
    pub const fn used_tokens(&self) -> u32 {
        self.used_tokens
    }

    /// Mark that the next `push_str` corresponds to the record at `line_idx`.
    /// Used to compute the `offset` field of the continuation handle on overflow.
    pub fn note_line(&mut self, line_idx: u32) {
        if !self.overflowed {
            self.last_line_noted = Some(line_idx);
        }
    }

    /// Append `s` to the body, capped at the budget. Once the budget is
    /// crossed, no further writes are accepted and the writer enters
    /// "overflowed" state until `finish_string` is called.
    pub fn push_str(&mut self, s: &str) {
        if self.overflowed {
            return;
        }
        let chunk = u32::try_from(approx_tokens(s)).unwrap_or(u32::MAX);
        if self.used_tokens.saturating_add(chunk) > self.budget_tokens {
            self.overflowed = true;
            return;
        }
        self.body.push_str(s);
        self.used_tokens = self.used_tokens.saturating_add(chunk);
    }

    /// Final body string. If the writer overflowed, appends the truncation
    /// sentinel (still JSON-parseable as a trailing object on its own line).
    ///
    /// # Panics
    /// Never panics in practice: `serde_json::to_string` only fails on types
    /// with non-string map keys, which the sentinel `json!({…})` literal
    /// never produces.
    #[must_use]
    pub fn finish_string(mut self) -> String {
        if self.overflowed {
            let mut cont_args = self.base_args.clone();
            if let Some(idx) = self.last_line_noted {
                cont_args["offset"] = json!(idx);
            }
            let sentinel = json!({
                "kind": "truncated",
                "emitted_tokens": self.used_tokens,
                "continuation": {
                    "tool": self.tool_name,
                    "args": cont_args,
                }
            });
            if !self.body.ends_with('\n') {
                self.body.push('\n');
            }
            self.body.push_str(
                &serde_json::to_string(&sentinel)
                    .expect("sentinel is a well-formed serde_json::Value"),
            );
        }
        self.body
    }
}
