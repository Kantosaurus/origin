// SPDX-License-Identifier: Apache-2.0
//! Speech-to-text dictation config and transcript interleave policy.
//!
//! Owns the dictation configuration and the queue/interleave policy that turns
//! a stream of partial and final transcripts into submittable prompt chunks.
//! The actual STT process is shelled out by the caller via an injected runner,
//! so this crate stays synchronous, side-effect free, and offline-testable.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can arise while validating dictation configuration.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum VoiceError {
    /// The configured STT command string was empty.
    #[error("dictation command must not be empty")]
    EmptyCommand,
}

/// Configuration for invoking an external speech-to-text engine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictationConfig {
    /// The STT executable to run, e.g. `whisper`.
    pub command: String,
    /// Base arguments passed to the executable, e.g. `["--model", "base"]`.
    pub args: Vec<String>,
    /// Optional spoken language hint, injected as `--language <value>`.
    pub language: Option<String>,
    /// Optional capture device, injected as `--device <value>`.
    pub device: Option<String>,
}

impl DictationConfig {
    /// Creates a configuration from a command and its base arguments.
    #[must_use]
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            language: None,
            device: None,
        }
    }
}

/// How transcripts are turned into submittable prompt chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DictationMode {
    /// Collect a full utterance and submit it only once a final transcript
    /// arrives.
    Queue,
    /// Stream each non-empty partial transcript into the prompt immediately.
    Interleave,
}

/// A single transcript emitted by the STT engine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transcript {
    /// The recognized text for this fragment.
    pub text: String,
    /// Whether this transcript is final (end of utterance) or partial.
    pub is_final: bool,
}

impl Transcript {
    /// Creates a transcript fragment.
    #[must_use]
    pub fn new(text: impl Into<String>, is_final: bool) -> Self {
        Self {
            text: text.into(),
            is_final,
        }
    }
}

/// Stateful accumulator that applies a [`DictationMode`] policy to transcripts.
#[derive(Debug, Clone)]
pub struct DictationSession {
    mode: DictationMode,
    buffer: String,
    ready: Option<String>,
}

impl DictationSession {
    /// Creates a new session that applies the given mode's policy.
    #[must_use]
    pub const fn new(mode: DictationMode) -> Self {
        Self {
            mode,
            buffer: String::new(),
            ready: None,
        }
    }

    /// Feeds a transcript into the session, advancing the policy state.
    ///
    /// In [`DictationMode::Queue`] the text is buffered and only marked ready
    /// once a final transcript arrives. In [`DictationMode::Interleave`] each
    /// non-empty fragment is marked ready immediately.
    // Takes `Transcript` by value to own the (often large) text without forcing
    // callers to keep the fragment alive; matches the published API shape.
    #[allow(clippy::needless_pass_by_value)]
    pub fn push(&mut self, t: Transcript) {
        match self.mode {
            DictationMode::Queue => {
                Self::append(&mut self.buffer, &t.text);
                if t.is_final {
                    let chunk = std::mem::take(&mut self.buffer);
                    if !chunk.is_empty() {
                        self.ready = Some(chunk);
                    }
                }
            }
            DictationMode::Interleave => {
                let trimmed = t.text.trim();
                if !trimmed.is_empty() {
                    self.ready = Some(trimmed.to_owned());
                }
            }
        }
    }

    /// Returns the next submittable chunk, if the policy has produced one.
    ///
    /// Consumes the chunk so a subsequent call returns `None` until more
    /// transcripts are pushed.
    pub const fn take_ready(&mut self) -> Option<String> {
        self.ready.take()
    }

    /// Returns the text currently buffered but not yet submittable.
    ///
    /// For [`DictationMode::Queue`] this is the in-progress utterance; for
    /// [`DictationMode::Interleave`] it is always empty since fragments are
    /// emitted eagerly.
    #[must_use]
    pub fn pending(&self) -> &str {
        &self.buffer
    }

    /// Appends `text` to `buffer`, inserting a separating space when needed.
    fn append(buffer: &mut String, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        if !buffer.is_empty() {
            buffer.push(' ');
        }
        buffer.push_str(trimmed);
    }
}

/// Resolves the full STT invocation argv from a configuration.
///
/// Returns the executable name plus the base arguments with `--language` and
/// `--device` flags appended when present.
#[must_use]
pub fn build_command(cfg: &DictationConfig) -> (String, Vec<String>) {
    let mut args = cfg.args.clone();
    if let Some(language) = &cfg.language {
        args.push("--language".to_owned());
        args.push(language.clone());
    }
    if let Some(device) = &cfg.device {
        args.push("--device".to_owned());
        args.push(device.clone());
    }
    (cfg.command.clone(), args)
}

/// Validates a dictation configuration before it is used to spawn a process.
///
/// # Errors
///
/// Returns [`VoiceError::EmptyCommand`] if the command is empty or whitespace.
pub fn validate(cfg: &DictationConfig) -> Result<(), VoiceError> {
    if cfg.command.trim().is_empty() {
        return Err(VoiceError::EmptyCommand);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn queue_buffers_until_final_then_yields_once() {
        let mut session = DictationSession::new(DictationMode::Queue);
        session.push(Transcript::new("hello", false));
        assert!(session.take_ready().is_none());
        assert_eq!(session.pending(), "hello");
        session.push(Transcript::new("world", true));
        assert_eq!(session.take_ready(), Some("hello world".to_owned()));
        // Already consumed; nothing left.
        assert!(session.take_ready().is_none());
        assert_eq!(session.pending(), "");
    }

    #[test]
    fn interleave_yields_each_non_empty_partial() {
        let mut session = DictationSession::new(DictationMode::Interleave);
        session.push(Transcript::new("one", false));
        assert_eq!(session.take_ready(), Some("one".to_owned()));
        session.push(Transcript::new("two", false));
        assert_eq!(session.take_ready(), Some("two".to_owned()));
        // Pending is always empty in interleave mode.
        assert_eq!(session.pending(), "");
    }

    #[test]
    fn interleave_skips_empty_partials() {
        let mut session = DictationSession::new(DictationMode::Interleave);
        session.push(Transcript::new("   ", false));
        assert!(session.take_ready().is_none());
        session.push(Transcript::new("real", false));
        assert_eq!(session.take_ready(), Some("real".to_owned()));
    }

    #[test]
    fn build_command_injects_language_and_device() {
        let cfg = DictationConfig {
            command: "whisper".to_owned(),
            args: vec!["--model".to_owned(), "base".to_owned()],
            language: Some("en".to_owned()),
            device: Some("mic0".to_owned()),
        };
        let (cmd, args) = build_command(&cfg);
        assert_eq!(cmd, "whisper");
        assert_eq!(
            args,
            vec![
                "--model".to_owned(),
                "base".to_owned(),
                "--language".to_owned(),
                "en".to_owned(),
                "--device".to_owned(),
                "mic0".to_owned(),
            ]
        );
    }

    #[test]
    fn build_command_omits_absent_flags() {
        let cfg = DictationConfig::new("whisper", vec!["--model".to_owned(), "base".to_owned()]);
        let (cmd, args) = build_command(&cfg);
        assert_eq!(cmd, "whisper");
        assert_eq!(args, vec!["--model".to_owned(), "base".to_owned()]);
    }

    #[test]
    fn validate_rejects_empty_command() {
        let cfg = DictationConfig::new("", Vec::new());
        assert_eq!(validate(&cfg), Err(VoiceError::EmptyCommand));
        let whitespace = DictationConfig::new("   ", Vec::new());
        assert_eq!(validate(&whitespace), Err(VoiceError::EmptyCommand));
    }

    #[test]
    fn validate_accepts_non_empty_command() {
        let cfg = DictationConfig::new("whisper", Vec::new());
        assert_eq!(validate(&cfg), Ok(()));
    }

    #[test]
    fn take_ready_none_when_nothing_ready() {
        let mut queue = DictationSession::new(DictationMode::Queue);
        assert!(queue.take_ready().is_none());
        let mut interleave = DictationSession::new(DictationMode::Interleave);
        assert!(interleave.take_ready().is_none());
    }

    #[test]
    fn queue_drops_empty_utterance() {
        let mut session = DictationSession::new(DictationMode::Queue);
        session.push(Transcript::new("   ", true));
        assert!(session.take_ready().is_none());
    }
}
