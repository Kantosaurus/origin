// SPDX-License-Identifier: Apache-2.0
//! Resume-after-error: pick up where the agent left off.
//!
//! When the user types a bare "continue" / "try again" right after a turn whose
//! visible output ended in an error, append a hint that points the model at the
//! recent error so it diagnoses and retries the failed step instead of treating
//! the word literally or restarting.
//!
//! Two guards keep this from hijacking ordinary prompts:
//! 1. The WHOLE message (trimmed) must be a short resume/retry phrase — so a real
//!    instruction like "continue editing main.rs" is untouched.
//! 2. The recent output must actually contain an error signal — so "continue"
//!    after a clean turn is byte-identical to before. Because BOTH must hold,
//!    the resume-phrase set can be generous without risking false augmentation.

/// How many recent scrollback lines to scan for an error signal.
///
/// A failed turn's error is normally near the end of its output, so a modest
/// window catches it without reaching back into an earlier, already-resolved turn.
pub const RESUME_SCAN_LINES: usize = 24;

/// True when the entire message is a short "resume / retry / keep going" phrase.
///
/// Matches on the whole string (trimmed, lowercased, trailing punctuation
/// stripped), so a multi-word instruction that merely *starts* with "continue"
/// is not matched.
#[must_use]
pub fn is_resume_intent(text: &str) -> bool {
    let t = text
        .trim()
        .trim_end_matches(['.', '!', '?', ',', ' '])
        .to_ascii_lowercase();
    matches!(
        t.as_str(),
        "continue"
            | "continue please"
            | "please continue"
            | "continue from where you left off"
            | "pick up where you left off"
            | "keep going"
            | "go on"
            | "carry on"
            | "proceed"
            | "go ahead"
            | "try again"
            | "tryagain"
            | "try that again"
            | "retry"
            | "redo"
            | "again"
            | "resume"
    )
}

/// The most recent error-looking line in `recent`, trimmed and length-capped.
///
/// `recent` is ordered most-recent-first; returns `None` when no recent line
/// reads as an error. Recognizes the TUI's `✘`/`✗`/`✖` error glyphs (what
/// `add_line("error> ", …)` paints) plus common failure words.
#[must_use]
pub fn last_error_excerpt(recent: &[String]) -> Option<String> {
    recent.iter().find(|l| line_is_error(l)).map(|l| {
        let s = l.trim();
        if s.chars().count() > 240 {
            let head: String = s.chars().take(240).collect();
            format!("{head}\u{2026}")
        } else {
            s.to_string()
        }
    })
}

/// Whether a single output line reads as an error.
///
/// The `✘`/`✗`/`✖` glyphs are the strong signal (the TUI's `error>` rows); the
/// word list catches provider/tool failures rendered as plain text. Deliberately
/// avoids a bare "error" substring (which would match prose like "error
/// handling") — requires `error:` / a leading `error` / explicit failure words.
fn line_is_error(line: &str) -> bool {
    if line.contains('\u{2718}') || line.contains('\u{2717}') || line.contains('\u{2716}') {
        return true;
    }
    let l = line.to_ascii_lowercase();
    let lt = l.trim_start();
    lt.starts_with("error")
        || l.contains("error:")
        || l.contains("failed")
        || l.contains("fatal")
        || l.contains("panic")
        || l.contains("traceback")
        || l.contains("exception")
        || l.contains("timed out")
        || l.contains("could not ")
        || l.contains("unable to ")
}

/// Append a resume hint to `text`, gated on a resume phrase + a recent error.
///
/// Returns `Some(augmented)` — `text` with a hint pointing at the most recent
/// error line appended — or `None` when either guard fails, in which case the
/// caller keeps the original prompt unchanged.
#[must_use]
pub fn augment_for_resume(text: &str, recent: &[String]) -> Option<String> {
    if !is_resume_intent(text) {
        return None;
    }
    let err = last_error_excerpt(recent)?;
    Some(format!(
        "{text}\n\n<resume-after-error>\nYour previous turn ended with an error — do NOT start the task over. Review your recent output, identify what failed, fix the cause, and retry the failed step to pick up exactly where you left off. The most recent error line was:\n{err}\n</resume-after-error>"
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn bare_resume_phrases_are_intents() {
        for p in [
            "continue",
            "Continue.",
            "  try again ",
            "Try Again!",
            "retry",
            "keep going",
            "resume",
            "go on",
            "proceed",
            "pick up where you left off",
        ] {
            assert!(is_resume_intent(p), "{p:?} should be a resume intent");
        }
    }

    #[test]
    fn real_instructions_are_not_intents() {
        for p in [
            "continue editing main.rs",
            "try again with a smaller batch size",
            "resume the download script",
            "what does continue do here?",
            "add a retry loop to the fetch",
            "",
            "go",
        ] {
            assert!(!is_resume_intent(p), "{p:?} must NOT be a resume intent");
        }
    }

    #[test]
    fn detects_error_glyph_and_words() {
        assert!(line_is_error("  \u{2718} build failed: linker error"));
        assert!(line_is_error("Error: connection refused"));
        assert!(line_is_error("  the command failed with exit code 1"));
        assert!(line_is_error("thread 'main' panicked at src/x.rs"));
        assert!(line_is_error("request timed out after 30s"));
        assert!(!line_is_error("here is some error handling code"));
        assert!(!line_is_error("  ◆ origin"));
        assert!(!line_is_error("the tests passed"));
    }

    #[test]
    fn last_error_excerpt_finds_most_recent() {
        // `recent` is most-recent-first; the first error line wins.
        let recent = vec![
            "you".to_string(),
            "continue".to_string(),
            "  \u{2718} cargo build failed".to_string(),
            "earlier ok line".to_string(),
        ];
        assert_eq!(
            last_error_excerpt(&recent).as_deref(),
            Some("\u{2718} cargo build failed")
        );
        assert!(last_error_excerpt(&["all good".to_string()]).is_none());
    }

    #[test]
    fn augments_only_when_intent_and_error_both_present() {
        let with_err = vec!["  \u{2718} build failed".to_string()];
        let clean = vec!["build succeeded".to_string()];

        // intent + error => augmented (carries the original text + the error line)
        let out = augment_for_resume("continue", &with_err).unwrap();
        assert!(out.starts_with("continue"));
        assert!(out.contains("resume-after-error"));
        assert!(out.contains("build failed"));

        // intent but no error => unchanged
        assert!(augment_for_resume("continue", &clean).is_none());
        // error but not a resume phrase => unchanged
        assert!(augment_for_resume("fix the build error please", &with_err).is_none());
    }
}
