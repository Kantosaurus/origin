// SPDX-License-Identifier: Apache-2.0
//! Structured tool error taxonomy.
//!
//! Every tool returns a [`ToolError`] instead of a free-form `String`; the
//! envelope serialises it as `{kind, message, recoverable, hint?}` so the
//! agent loop can pattern-match recoverable failures without LLM re-parsing.

use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrClass {
    Io,
    Edit,
    Bash,
    Regex,
    Budget,
    Subsystem,
    Validation,
}

impl ErrClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::Edit => "edit",
            Self::Bash => "bash",
            Self::Regex => "regex",
            Self::Budget => "budget",
            Self::Subsystem => "subsystem",
            Self::Validation => "validation",
        }
    }
}

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct ToolError {
    pub class: ErrClass,
    pub reason: &'static str,
    pub message: String,
    pub recoverable: bool,
    pub hint: Option<String>,
}

impl ToolError {
    #[must_use]
    pub fn new(class: ErrClass, reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            class,
            reason,
            message: message.into(),
            recoverable: false,
            hint: None,
        }
    }

    #[must_use]
    pub const fn recoverable(mut self, yes: bool) -> Self {
        self.recoverable = yes;
        self
    }

    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut obj = json!({
            "kind": format!("{}.{}", self.class.as_str(), self.reason),
            "message": self.message,
            "recoverable": self.recoverable,
        });
        if let Some(h) = &self.hint {
            obj["hint"] = Value::String(h.clone());
        }
        obj
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_with_kind_and_recoverable() {
        let err = ToolError::new(ErrClass::Edit, "no_match", "string not found")
            .recoverable(true)
            .hint("widen the context");
        let json = err.to_json();
        assert_eq!(json["kind"], "edit.no_match");
        assert_eq!(json["message"], "string not found");
        assert_eq!(json["recoverable"], true);
        assert_eq!(json["hint"], "widen the context");
    }

    #[test]
    fn classes_match_taxonomy() {
        for class in [
            ErrClass::Io,
            ErrClass::Edit,
            ErrClass::Bash,
            ErrClass::Regex,
            ErrClass::Budget,
            ErrClass::Subsystem,
            ErrClass::Validation,
        ] {
            let s: &'static str = class.as_str();
            assert!(!s.is_empty());
            assert!(!s.contains('.'));
        }
    }
}
