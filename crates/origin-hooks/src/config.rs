// SPDX-License-Identifier: Apache-2.0
//! Hook configuration loading.
//!
//! The daemon loads a JSON file (by convention `~/.origin/hooks.json`) that maps
//! [`LifecycleEvent`](crate::event::LifecycleEvent) kinds to shell programs. Each
//! configured event gets a pre-spawned [`ShellPool`](crate::shellpool::ShellPool)
//! the daemon dispatches the serialized event to.
//!
//! A **missing or empty file means no hooks** — [`HooksConfig::load`] returns an
//! empty config, the daemon spawns no pools, and the agent path is byte-identical
//! to running without the hooks subsystem at all.
//!
//! Example `hooks.json`:
//! ```json
//! {
//!   "hooks": [
//!     { "event": "pre_tool",      "program": "/usr/local/bin/guard", "args": ["--strict"] },
//!     { "event": "session_start", "program": "node", "args": ["hooks/on-start.js"], "pool_size": 1 }
//!   ]
//! }
//! ```
//!
//! Each hook program is a **long-lived NUL-framed responder** (the
//! [`ShellPool`](crate::shellpool::ShellPool) contract): it reads one
//! event-JSON line on stdin and writes one JSON response terminated by a NUL
//! byte on stdout, looping for the life of the daemon.

// `HooksConfig` / `ConfigError` intentionally repeat the module name so callers
// read `origin_hooks::HooksConfig` without disambiguating the module.
#![allow(clippy::module_name_repetitions)]

use std::path::Path;

use serde::{Deserialize, Deserializer};

use crate::event::LifecycleEvent;
use crate::shellpool::ShellSpec;

/// The kind of lifecycle event a hook subscribes to.
///
/// The canonical serialized form is the `snake_case` tag of
/// [`LifecycleEvent`](crate::event::LifecycleEvent). For drop-in compatibility
/// with a Claude `hooks.json`, [`HookEventKind::from_label`] also accepts the
/// Claude event names (`PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `Stop`,
/// `PreCompact`, `Notification`, …) as aliases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookEventKind {
    /// Before a user prompt is processed.
    PrePrompt,
    /// After a user prompt completes.
    PostPrompt,
    /// Before a tool dispatches (override-capable: a `Deny` skips the tool).
    PreTool,
    /// After a tool completes (informational).
    PostTool,
    /// Before a commit (informational).
    PreCommit,
    /// After a commit (informational).
    PostCommit,
    /// Once when a session starts.
    SessionStart,
    /// Once when a session ends.
    SessionEnd,
    /// Before an assistant message is displayed (transform/hide capable).
    MessageDisplay,
    /// Just before the provider model call for a turn (informational).
    BeforeModel,
    /// Just after the provider model call returns (informational).
    AfterModel,
    /// Just before transcript compaction runs (informational).
    PreCompress,
    /// A side-band notification (informational).
    Notification,
}

impl HookEventKind {
    /// Parse an event-kind label, accepting both the canonical origin
    /// `snake_case` names and Claude-compatible aliases.
    ///
    /// Matching is case-insensitive and ignores surrounding whitespace, so a
    /// Claude `hooks.json` (which uses names like `PreToolUse`) loads unchanged.
    /// Returns `None` for an unrecognised label.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        // Each arm lists the canonical origin `snake_case` name first, then any
        // Claude-compat aliases that map onto the same kind, so a Claude
        // `hooks.json` loads unchanged.
        match label.trim().to_ascii_lowercase().as_str() {
            "pre_prompt" | "userpromptsubmit" => Some(Self::PrePrompt),
            "post_prompt" | "stop" => Some(Self::PostPrompt),
            "pre_tool" | "pretooluse" => Some(Self::PreTool),
            "post_tool" | "posttooluse" => Some(Self::PostTool),
            "pre_commit" => Some(Self::PreCommit),
            "post_commit" => Some(Self::PostCommit),
            "session_start" | "sessionstart" => Some(Self::SessionStart),
            "session_end" | "sessionend" => Some(Self::SessionEnd),
            "message_display" => Some(Self::MessageDisplay),
            "before_model" => Some(Self::BeforeModel),
            "after_model" => Some(Self::AfterModel),
            "pre_compress" | "precompact" => Some(Self::PreCompress),
            "notification" => Some(Self::Notification),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for HookEventKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let label = String::deserialize(deserializer)?;
        Self::from_label(&label)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown hook event kind `{label}`")))
    }
}

impl LifecycleEvent {
    /// The [`HookEventKind`] this event dispatches to.
    #[must_use]
    pub const fn kind(&self) -> HookEventKind {
        match self {
            Self::PrePrompt { .. } => HookEventKind::PrePrompt,
            Self::PostPrompt { .. } => HookEventKind::PostPrompt,
            Self::PreTool { .. } => HookEventKind::PreTool,
            Self::PostTool { .. } => HookEventKind::PostTool,
            Self::PreCommit { .. } => HookEventKind::PreCommit,
            Self::PostCommit { .. } => HookEventKind::PostCommit,
            Self::SessionStart => HookEventKind::SessionStart,
            Self::SessionEnd => HookEventKind::SessionEnd,
            Self::MessageDisplay { .. } => HookEventKind::MessageDisplay,
            Self::BeforeModel { .. } => HookEventKind::BeforeModel,
            Self::AfterModel { .. } => HookEventKind::AfterModel,
            Self::PreCompress { .. } => HookEventKind::PreCompress,
            Self::Notification { .. } => HookEventKind::Notification,
        }
    }
}

/// Default shell-pool size for a hook when unspecified.
const fn default_pool_size() -> usize {
    1
}

/// One configured hook: an event kind bound to a shell program.
#[derive(Debug, Clone, Deserialize)]
pub struct HookEntry {
    /// Which lifecycle event triggers this hook.
    pub event: HookEventKind,
    /// Program to spawn (resolved on `PATH` or an absolute path).
    pub program: String,
    /// Arguments passed to the program at spawn time.
    #[serde(default)]
    pub args: Vec<String>,
    /// How many pre-spawned workers to keep for this hook.
    #[serde(default = "default_pool_size")]
    pub pool_size: usize,
}

impl HookEntry {
    /// The [`ShellSpec`] to pre-spawn this hook's pool. The framing terminator is
    /// always NUL, the pool's standardized response boundary.
    #[must_use]
    pub fn spec(&self) -> ShellSpec {
        ShellSpec {
            program: self.program.clone(),
            args: self.args.clone(),
            read_terminator: 0,
        }
    }

    /// Effective pool size, clamped to at least 1.
    #[must_use]
    pub const fn effective_pool_size(&self) -> usize {
        if self.pool_size == 0 {
            1
        } else {
            self.pool_size
        }
    }
}

/// The parsed hooks configuration.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HooksConfig {
    /// All configured hooks, in file order.
    #[serde(default)]
    pub hooks: Vec<HookEntry>,
}

/// Errors loading a [`HooksConfig`].
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Reading the file failed (other than not-found, which is not an error).
    #[error("io: {0}")]
    Io(String),
    /// The file was not valid JSON for the schema.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

impl HooksConfig {
    /// Load a config from `path`.
    ///
    /// A **missing file** yields an empty config (`Ok`) — hooks are simply off.
    /// Only a genuine read error or malformed JSON is surfaced.
    ///
    /// # Errors
    /// [`ConfigError::Io`] on a non-not-found read error; [`ConfigError::Json`]
    /// when the file is present but not valid for the schema.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::from_json_str(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ConfigError::Io(e.to_string())),
        }
    }

    /// Parse a config from a JSON string. Empty / whitespace ⇒ empty config.
    ///
    /// # Errors
    /// [`ConfigError::Json`] when `s` is non-empty but not valid for the schema.
    pub fn from_json_str(s: &str) -> Result<Self, ConfigError> {
        if s.trim().is_empty() {
            return Ok(Self::default());
        }
        Ok(serde_json::from_str(s)?)
    }

    /// True when no hooks are configured (the common, byte-identical case).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    /// All hooks subscribed to `kind`, in file order.
    pub fn entries_for(&self, kind: HookEventKind) -> impl Iterator<Item = &HookEntry> {
        self.hooks.iter().filter(move |h| h.event == kind)
    }

    /// Whether any hook subscribes to `kind` (lets the daemon skip spawning a
    /// pool for events nobody listens to).
    #[must_use]
    pub fn has_event(&self, kind: HookEventKind) -> bool {
        self.hooks.iter().any(|h| h.event == kind)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_is_empty_config() {
        assert!(HooksConfig::from_json_str("").unwrap().is_empty());
        assert!(HooksConfig::from_json_str("   \n").unwrap().is_empty());
    }

    #[test]
    fn missing_file_loads_empty() {
        let cfg = HooksConfig::load(Path::new("/no/such/hooks-xyz.json")).unwrap();
        assert!(cfg.is_empty());
    }

    #[test]
    fn parses_entries_and_filters_by_event() {
        let json = r#"{
            "hooks": [
                { "event": "pre_tool", "program": "guard", "args": ["--strict"] },
                { "event": "session_start", "program": "on-start.sh" },
                { "event": "pre_tool", "program": "second-guard" }
            ]
        }"#;
        let cfg = HooksConfig::from_json_str(json).unwrap();
        assert_eq!(cfg.hooks.len(), 3);
        assert!(cfg.has_event(HookEventKind::PreTool));
        assert!(cfg.has_event(HookEventKind::SessionStart));
        assert!(!cfg.has_event(HookEventKind::PostTool));
        let pre: Vec<_> = cfg.entries_for(HookEventKind::PreTool).collect();
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0].program, "guard");
        assert_eq!(pre[0].args, vec!["--strict".to_string()]);
        // Default pool size applies when omitted.
        assert_eq!(pre[0].effective_pool_size(), 1);
    }

    #[test]
    fn spec_uses_nul_terminator() {
        let e = HookEntry {
            event: HookEventKind::PostTool,
            program: "p".into(),
            args: vec![],
            pool_size: 0,
        };
        let spec = e.spec();
        assert_eq!(spec.read_terminator, 0);
        // Zero pool size clamps to one worker.
        assert_eq!(e.effective_pool_size(), 1);
    }

    #[test]
    fn lifecycle_event_kind_maps_each_variant() {
        assert_eq!(LifecycleEvent::SessionStart.kind(), HookEventKind::SessionStart);
        assert_eq!(LifecycleEvent::SessionEnd.kind(), HookEventKind::SessionEnd);
        assert_eq!(
            LifecycleEvent::MessageDisplay { text: String::new() }.kind(),
            HookEventKind::MessageDisplay
        );
        assert_eq!(
            LifecycleEvent::PrePrompt { text: String::new() }.kind(),
            HookEventKind::PrePrompt
        );
        assert_eq!(
            LifecycleEvent::BeforeModel { model: String::new() }.kind(),
            HookEventKind::BeforeModel
        );
        assert_eq!(
            LifecycleEvent::AfterModel { model: String::new() }.kind(),
            HookEventKind::AfterModel
        );
        assert_eq!(
            LifecycleEvent::PreCompress { current_bytes: 0 }.kind(),
            HookEventKind::PreCompress
        );
        assert_eq!(
            LifecycleEvent::Notification {
                message: String::new()
            }
            .kind(),
            HookEventKind::Notification
        );
    }

    #[test]
    fn from_label_accepts_canonical_origin_names() {
        assert_eq!(
            HookEventKind::from_label("pre_tool"),
            Some(HookEventKind::PreTool)
        );
        assert_eq!(
            HookEventKind::from_label("message_display"),
            Some(HookEventKind::MessageDisplay)
        );
        assert_eq!(
            HookEventKind::from_label("before_model"),
            Some(HookEventKind::BeforeModel)
        );
        assert_eq!(
            HookEventKind::from_label("pre_compress"),
            Some(HookEventKind::PreCompress)
        );
        assert_eq!(HookEventKind::from_label("nope"), None);
    }

    #[test]
    fn from_label_accepts_claude_aliases() {
        // Claude event names map onto the equivalent origin event kinds.
        assert_eq!(
            HookEventKind::from_label("PreToolUse"),
            Some(HookEventKind::PreTool)
        );
        assert_eq!(
            HookEventKind::from_label("PostToolUse"),
            Some(HookEventKind::PostTool)
        );
        assert_eq!(
            HookEventKind::from_label("UserPromptSubmit"),
            Some(HookEventKind::PrePrompt)
        );
        assert_eq!(HookEventKind::from_label("Stop"), Some(HookEventKind::PostPrompt));
        assert_eq!(
            HookEventKind::from_label("PreCompact"),
            Some(HookEventKind::PreCompress)
        );
        assert_eq!(
            HookEventKind::from_label("Notification"),
            Some(HookEventKind::Notification)
        );
        // Case- and whitespace-insensitive.
        assert_eq!(
            HookEventKind::from_label("  pretooluse "),
            Some(HookEventKind::PreTool)
        );
    }

    #[test]
    fn config_parses_claude_event_names() {
        // A Claude-style hooks.json loads without rewriting event names.
        let json = r#"{
            "hooks": [
                { "event": "PreToolUse", "program": "guard" },
                { "event": "Stop", "program": "on-stop.sh" },
                { "event": "PreCompact", "program": "on-compact.sh" }
            ]
        }"#;
        let cfg = HooksConfig::from_json_str(json).unwrap();
        assert!(cfg.has_event(HookEventKind::PreTool));
        assert!(cfg.has_event(HookEventKind::PostPrompt));
        assert!(cfg.has_event(HookEventKind::PreCompress));
    }
}
