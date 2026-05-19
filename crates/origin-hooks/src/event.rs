//! Typed lifecycle events + hook stdout override schema.
//!
//! Events serialize to JSON for hook stdin; hook stdout JSON is parsed back
//! into [`HookOverride`].

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Lifecycle event emitted by the daemon for each hook to inspect.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LifecycleEvent {
    PrePrompt { text: String },
    PostPrompt { text: String },
    PreTool { tool: String, args_preview: String },
    PostTool { tool: String, phase: ToolPhase },
    PreCommit { branch: String },
    PostCommit { sha: String },
    SessionStart,
    SessionEnd,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPhase {
    Ok,
    Err,
    Skipped,
}

/// Override decision parsed from a hook's stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HookOverrideInner {
    Allow { reason: String },
    Deny { reason: String },
    Mutate { patch: String },
}

#[derive(Debug, Clone)]
pub enum HookOverride {
    Passthrough,
    Allow { reason: String },
    Deny { reason: String },
    Mutate { patch: String },
}

#[derive(Debug, Error)]
pub enum HookParseError {
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    r#override: Option<HookOverrideInner>,
}

/// Parse the bytes a hook printed on stdout into a [`HookOverride`].
///
/// Empty stdout means the hook is signalling "no opinion" → [`HookOverride::Passthrough`].
///
/// # Errors
/// Returns [`HookParseError::Json`] if non-empty stdout is not valid JSON.
pub fn parse_hook_stdout(bytes: &[u8]) -> Result<HookOverride, HookParseError> {
    let trimmed = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map_or(&[][..], |i| &bytes[i..]);
    if trimmed.is_empty() {
        return Ok(HookOverride::Passthrough);
    }
    let env: Envelope = serde_json::from_slice(trimmed)?;
    Ok(match env.r#override {
        None => HookOverride::Passthrough,
        Some(HookOverrideInner::Allow { reason }) => HookOverride::Allow { reason },
        Some(HookOverrideInner::Deny { reason }) => HookOverride::Deny { reason },
        Some(HookOverrideInner::Mutate { patch }) => HookOverride::Mutate { patch },
    })
}
