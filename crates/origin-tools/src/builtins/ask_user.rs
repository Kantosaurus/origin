// SPDX-License-Identifier: Apache-2.0
//! `ask_user` — put a structured single/multi-select question to the human.
//!
//! This is the agent-facing half of the interactive-prompt capability. The tool
//! parses a `{question, options, multi_select?, allow_custom?}` payload; the
//! *interactive* pause-await (emit `ChoiceAsk` → block on `ChoiceDecision`) is
//! owned by the daemon's dispatch path, which has the IPC protocol + the
//! daemon-wide choice registry. This module stays dependency-free so the builtin
//! crate keeps no daemon dependency:
//!
//! - [`AskUserArgs::from_value`] validates the JSON the model sent.
//! - [`backward_compat_instruction`] renders the prose fallback returned when no
//!   interactive channel is wired (headless / swarm / unit tests), telling the
//!   model to ask the question in prose instead.
//! - [`format_decision`] folds a resolved decision (chosen labels + optional
//!   free text) into the string the daemon returns as the tool result, and
//!   handles the empty/cancelled "user skipped" case.
//!
//! `ask_user` is `AutoAllowed` + `Pure`: it never mutates the workspace and it
//! is asking *the user*, so it must not itself trigger a permission prompt.

#![allow(clippy::module_name_repetitions)] // `AskUserArgs` mirrors sibling `TaskInput` precedent

use serde::Deserialize;

/// One selectable option in an [`AskUserArgs`].
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AskOption {
    /// The selectable label, returned verbatim to the model when chosen.
    pub label: String,
    /// Optional secondary help text shown beneath/after the label.
    #[serde(default)]
    pub description: Option<String>,
}

/// Default for [`AskUserArgs::allow_custom`] — the picker offers a free-text
/// "type your own" affordance unless the model explicitly disables it.
const fn default_allow_custom() -> bool {
    true
}

/// Parsed `{question, options, multi_select?, allow_custom?}` input for the
/// `ask_user` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct AskUserArgs {
    /// The question to put to the user.
    pub question: String,
    /// The selectable options, in display order. May be empty when the model
    /// only wants free-text (`allow_custom` then carries the interaction).
    #[serde(default)]
    pub options: Vec<AskOption>,
    /// `true` ⇒ the user may select more than one option.
    #[serde(default)]
    pub multi_select: bool,
    /// `true` (the default) ⇒ the picker offers a "type your own" affordance.
    #[serde(default = "default_allow_custom")]
    pub allow_custom: bool,
}

impl AskUserArgs {
    /// Validate and parse the model's JSON input.
    ///
    /// # Errors
    /// Returns a human-readable message when `question` is missing/blank or the
    /// JSON shape is wrong, so the daemon can surface it as a `BadArgs` error.
    pub fn from_value(v: &serde_json::Value) -> Result<Self, String> {
        let args: Self =
            serde_json::from_value(v.clone()).map_err(|e| format!("ask_user: invalid input: {e}"))?;
        if args.question.trim().is_empty() {
            return Err("ask_user: `question` must be a non-empty string".to_string());
        }
        Ok(args)
    }

    /// Borrow the option labels in display order. Used to map a decision's
    /// selected indices back to the labels the model receives.
    #[must_use]
    pub fn labels(&self) -> Vec<&str> {
        self.options.iter().map(|o| o.label.as_str()).collect()
    }
}

/// Render the backward-compatible prose instruction returned when no interactive
/// channel is available (a headless run, a swarm sub-agent, or a unit test).
///
/// Rather than silently hanging or skipping, the tool returns text telling the
/// model to ask the question conversationally, listing the options it would have
/// rendered, so the human can still answer in their next message.
#[must_use]
pub fn backward_compat_instruction(args: &AskUserArgs) -> String {
    let mut out = String::new();
    out.push_str(
        "Interactive prompting is not available in this session. Ask the user this \
         question in your next message (in prose) and wait for their reply:\n\n",
    );
    out.push_str(&args.question);
    if !args.options.is_empty() {
        use std::fmt::Write as _;
        out.push_str("\n\nOptions:");
        for (i, opt) in args.options.iter().enumerate() {
            // Writing into a `String` is infallible, so the `Result` is discarded.
            let _ = write!(out, "\n  {}. {}", i + 1, opt.label);
            if let Some(d) = &opt.description {
                if !d.is_empty() {
                    let _ = write!(out, " — {d}");
                }
            }
        }
    }
    if args.multi_select {
        out.push_str("\n\n(The user may pick more than one.)");
    }
    if args.allow_custom {
        out.push_str("\n\n(The user may also answer with something not listed.)");
    }
    out
}

/// Fold a resolved decision into the tool-result string the daemon returns to
/// the model.
///
/// `chosen_labels` are the labels for the selected option indices (already
/// mapped by the daemon from `ChoiceDecision.selected`), and `custom` is any
/// free-text the user typed. An empty result (no labels and no custom text) is
/// the cancelled/skipped path and yields an explicit "user skipped" message so
/// the model can react gracefully rather than seeing an empty string.
#[must_use]
pub fn format_decision(chosen_labels: &[String], custom: Option<&str>) -> String {
    let custom = custom.map(str::trim).filter(|s| !s.is_empty());
    let has_labels = !chosen_labels.is_empty();
    if !has_labels && custom.is_none() {
        return "The user skipped this question (no selection).".to_string();
    }
    let mut parts: Vec<String> = chosen_labels.to_vec();
    if let Some(c) = custom {
        parts.push(c.to_string());
    }
    if parts.len() == 1 {
        format!("The user chose: {}", parts[0])
    } else {
        format!("The user chose: {}", parts.join(", "))
    }
}

crate::origin_tool! {
    name: "ask_user",
    description: "Put a structured single/multi-select question to the user and return their choice(s). Use when you need a decision from the human (which option, which file, yes/no with reasons) rather than guessing. Options each have a label and optional description; set multi_select to allow several picks; allow_custom (default true) lets the user type a free-text answer.",
    tier: crate::Tier::AutoAllowed,
    urgency: crate::Urgency::High,
    side_effects: crate::SideEffects::Pure,
    input_schema: r#"{"type":"object","required":["question","options"],"properties":{"question":{"type":"string"},"options":{"type":"array","items":{"type":"object","required":["label"],"properties":{"label":{"type":"string"},"description":{"type":"string"}}}},"multi_select":{"type":"boolean"},"allow_custom":{"type":"boolean"}}}"#,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    fn args(json: &serde_json::Value) -> AskUserArgs {
        AskUserArgs::from_value(json).expect("valid args")
    }

    #[test]
    fn parses_full_input() {
        let a = args(&serde_json::json!({
            "question": "Pick a branch",
            "options": [
                {"label": "main", "description": "the trunk"},
                {"label": "dev"}
            ],
            "multi_select": true,
            "allow_custom": false
        }));
        assert_eq!(a.question, "Pick a branch");
        assert_eq!(a.options.len(), 2);
        assert_eq!(a.options[0].description.as_deref(), Some("the trunk"));
        assert!(a.options[1].description.is_none());
        assert!(a.multi_select);
        assert!(!a.allow_custom);
        assert_eq!(a.labels(), vec!["main", "dev"]);
    }

    #[test]
    fn allow_custom_defaults_true_multi_select_false() {
        let a = args(&serde_json::json!({
            "question": "Continue?",
            "options": [{"label": "yes"}, {"label": "no"}]
        }));
        assert!(a.allow_custom, "allow_custom defaults to true");
        assert!(!a.multi_select, "multi_select defaults to false");
    }

    #[test]
    fn blank_question_is_rejected() {
        let err = AskUserArgs::from_value(&serde_json::json!({
            "question": "   ",
            "options": []
        }))
        .expect_err("blank question is rejected");
        assert!(err.contains("question"), "error mentions question: {err}");
    }

    #[test]
    fn backward_compat_lists_options_and_question() {
        let a = args(&serde_json::json!({
            "question": "Which env?",
            "options": [
                {"label": "staging", "description": "sandbox"},
                {"label": "prod"}
            ],
            "multi_select": true
        }));
        let text = backward_compat_instruction(&a);
        assert!(text.contains("Which env?"), "includes the question");
        assert!(text.contains("1. staging"), "numbers the first option");
        assert!(text.contains("— sandbox"), "renders the description");
        assert!(text.contains("2. prod"), "numbers the second option");
        assert!(text.contains("pick more than one"), "notes multi-select");
        assert!(
            text.contains("not listed"),
            "notes the custom affordance (default on)"
        );
    }

    #[test]
    fn format_decision_single_label() {
        let out = format_decision(&["main".to_string()], None);
        assert_eq!(out, "The user chose: main");
    }

    #[test]
    fn format_decision_multi_labels() {
        let out = format_decision(&["a".to_string(), "b".to_string()], None);
        assert_eq!(out, "The user chose: a, b");
    }

    #[test]
    fn format_decision_custom_appended() {
        let out = format_decision(&["a".to_string()], Some("something else"));
        assert_eq!(out, "The user chose: a, something else");
    }

    #[test]
    fn format_decision_custom_only() {
        let out = format_decision(&[], Some("freeform answer"));
        assert_eq!(out, "The user chose: freeform answer");
    }

    #[test]
    fn format_decision_empty_is_skip() {
        let out = format_decision(&[], None);
        assert!(out.contains("skipped"), "empty decision is a skip: {out}");
    }

    #[test]
    fn format_decision_blank_custom_is_skip() {
        // Whitespace-only custom is treated as no answer.
        let out = format_decision(&[], Some("   "));
        assert!(out.contains("skipped"), "blank custom is a skip: {out}");
    }
}
