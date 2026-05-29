// SPDX-License-Identifier: Apache-2.0
//! Live suggestion engine for the TUI input buffer.
//!
//! Computes ranked candidates on every keystroke when the **trailing
//! token** of the buffer matches a completable prefix (`/`, `/-`, or
//! `{workflow:`). The trailing token is the substring after the last
//! whitespace character, so `/` is recognized mid-prompt — e.g. typing
//! "please run /fro" surfaces `/frontend-design` suggestions just like
//! a bare `/fro` does.
//!
//! Pure — no I/O; the caller passes a [`CompletionSources`] snapshot.

use crate::autocomplete::CompletionSources;

const MAX_VISIBLE: usize = 6;

#[derive(Debug, Clone, Default)]
pub struct SuggestionState {
    /// Wrapped candidate strings (already includes the leading `/`, `/-`,
    /// or `{workflow:` syntax so they can be rendered verbatim).
    pub candidates: Vec<String>,
    /// Ghost text shown after the cursor when there's a single unique
    /// match. Empty when ambiguous, no match, or popup not open.
    pub ghost: String,
    /// Currently-selected candidate index (0-based). Drives the popup
    /// highlight and `accept_selected`. Always `< candidates.len()` when
    /// `candidates` is non-empty.
    pub selected: usize,
    /// Byte length of the buffer **prefix** that precedes the trailing
    /// token being completed. Accepting a candidate replaces
    /// `buffer[prefix_len..]` with the chosen candidate. `0` when the
    /// completion covers the whole buffer.
    pub prefix_len: usize,
}

#[must_use]
pub fn suggest(buffer: &str, sources: &CompletionSources) -> SuggestionState {
    let (prefix_len, token) = trailing_token(buffer);
    if let Some(partial) = token.strip_prefix("{workflow:") {
        let partial = partial.strip_suffix('}').unwrap_or(partial);
        return match_candidates(token, partial, prefix_len, &sources.workflows, |full| {
            format!("{{workflow:{full}}}")
        });
    }
    if let Some(partial) = token.strip_prefix("/-") {
        return match_candidates(token, partial, prefix_len, &sources.skills, |full| {
            format!("/-{full}")
        });
    }
    if let Some(partial) = token.strip_prefix('/') {
        return match_candidates(token, partial, prefix_len, &sources.skills, |full| {
            format!("/{full}")
        });
    }
    SuggestionState::default()
}

/// Split `buffer` into (`prefix_len`, `token`) where `token` is the
/// substring after the last whitespace character. For an empty buffer
/// or a buffer ending in whitespace, `token` is `""`. The whole buffer
/// becomes the token when there is no whitespace.
fn trailing_token(buffer: &str) -> (usize, &str) {
    // Find the byte index of the last whitespace char; the token starts
    // immediately after it (or at 0 when none is found).
    buffer.rfind(char::is_whitespace).map_or((0, buffer), |idx| {
        // `idx` is the byte index of the whitespace char; advance past it.
        let next = idx + buffer[idx..].chars().next().map_or(1, char::len_utf8);
        (next, &buffer[next..])
    })
}

fn match_candidates(
    token: &str,
    partial: &str,
    prefix_len: usize,
    names: &[String],
    wrap: impl Fn(&str) -> String,
) -> SuggestionState {
    let mut matches: Vec<String> = names
        .iter()
        .filter(|c| c.starts_with(partial))
        .map(|m| wrap(m))
        .collect();
    matches.sort();
    matches.truncate(MAX_VISIBLE);

    // Ghost text mirrors `unique_match_produces_ghost`: only when there's
    // exactly one match AND it extends the current trailing token.
    let ghost = if matches.len() == 1 && matches[0].starts_with(token) && matches[0].len() > token.len() {
        matches[0][token.len()..].to_string()
    } else {
        String::new()
    };

    SuggestionState {
        candidates: matches,
        ghost,
        selected: 0,
        prefix_len,
    }
}

/// Apply the currently-selected candidate to `buffer`, replacing the
/// trailing token in place. No-op when there are no candidates.
pub fn accept_selected(state: &SuggestionState, buffer: &mut String) {
    if state.candidates.is_empty() {
        return;
    }
    let idx = state.selected.min(state.candidates.len() - 1);
    buffer.truncate(state.prefix_len);
    buffer.push_str(&state.candidates[idx]);
}

/// Advance the popup selection. Wraps at the bottom. No-op when there
/// are no candidates.
pub fn select_next(state: &mut SuggestionState) {
    if state.candidates.is_empty() {
        return;
    }
    state.selected = (state.selected + 1) % state.candidates.len();
}

/// Move the popup selection up by one. Wraps at the top. No-op when
/// there are no candidates.
pub fn select_prev(state: &mut SuggestionState) {
    if state.candidates.is_empty() {
        return;
    }
    state.selected = if state.selected == 0 {
        state.candidates.len() - 1
    } else {
        state.selected - 1
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autocomplete::CompletionSources;

    fn srcs() -> CompletionSources {
        CompletionSources {
            skills: vec![
                "frontend-design".into(),
                "frontend-design:frontend-design".into(),
                "impeccable".into(),
                "polish".into(),
            ],
            workflows: vec!["frontend-design".into(), "polish-pass".into()],
        }
    }

    #[test]
    fn slash_prefix_returns_sorted_candidates() {
        let s = suggest("/fro", &srcs());
        assert_eq!(s.candidates.len(), 2);
        assert_eq!(s.candidates[0], "/frontend-design");
        assert_eq!(s.candidates[1], "/frontend-design:frontend-design");
        assert_eq!(s.selected, 0);
        assert_eq!(s.prefix_len, 0);
    }

    #[test]
    fn unique_match_produces_ghost() {
        let s = suggest("/impe", &srcs());
        assert_eq!(s.candidates.len(), 1);
        assert_eq!(s.candidates[0], "/impeccable");
        assert_eq!(s.ghost, "ccable");
    }

    #[test]
    fn multiple_matches_no_ghost() {
        let s = suggest("/fro", &srcs());
        assert!(s.ghost.is_empty());
    }

    #[test]
    fn no_prefix_returns_empty() {
        let s = suggest("hello", &srcs());
        assert!(s.candidates.is_empty());
    }

    #[test]
    fn workflow_prefix_matches() {
        let s = suggest("{workflow:polish", &srcs());
        assert_eq!(s.candidates.len(), 1);
        assert_eq!(s.candidates[0], "{workflow:polish-pass}");
    }

    #[test]
    fn deactivate_prefix_matches() {
        let s = suggest("/-impe", &srcs());
        assert_eq!(s.candidates.len(), 1);
        assert_eq!(s.candidates[0], "/-impeccable");
    }

    #[test]
    fn bare_slash_lists_all_skills() {
        let s = suggest("/", &srcs());
        assert_eq!(s.candidates.len(), 4);
    }

    #[test]
    fn empty_buffer_returns_empty() {
        let s = suggest("", &srcs());
        assert!(s.candidates.is_empty());
    }

    /// `/foo bar` is two whitespace-separated tokens; the trailing token
    /// `"bar"` doesn't start with `/`, so no suggestions fire.
    #[test]
    fn slash_with_whitespace_after_returns_empty() {
        let s = suggest("/foo bar", &srcs());
        assert!(s.candidates.is_empty());
    }

    /// Regression for the reported bug: typing `/` after some prompt
    /// text must surface the skill popup against the trailing token.
    #[test]
    fn slash_mid_prompt_triggers_suggestions() {
        let s = suggest("please run /fro", &srcs());
        assert_eq!(s.candidates.len(), 2);
        assert_eq!(s.candidates[0], "/frontend-design");
        // The trailing token starts after "please run " (11 bytes).
        assert_eq!(s.prefix_len, "please run ".len());
    }

    #[test]
    fn slash_mid_prompt_bare_lists_all() {
        let s = suggest("hello /", &srcs());
        assert_eq!(s.candidates.len(), 4);
    }

    #[test]
    fn workflow_mid_prompt_triggers_suggestions() {
        let s = suggest("do {workflow:pol", &srcs());
        assert_eq!(s.candidates.len(), 1);
        assert_eq!(s.candidates[0], "{workflow:polish-pass}");
        assert_eq!(s.prefix_len, "do ".len());
    }

    #[test]
    fn select_next_wraps() {
        let mut s = suggest("/fro", &srcs());
        assert_eq!(s.selected, 0);
        select_next(&mut s);
        assert_eq!(s.selected, 1);
        select_next(&mut s);
        // Two candidates → wrap back to 0.
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn select_prev_wraps() {
        let mut s = suggest("/fro", &srcs());
        assert_eq!(s.selected, 0);
        select_prev(&mut s);
        // From 0 → wraps to last (index 1 of 2).
        assert_eq!(s.selected, 1);
    }

    #[test]
    fn accept_selected_replaces_trailing_token() {
        let mut buf = "please run /fro".to_string();
        let mut s = suggest(&buf, &srcs());
        select_next(&mut s); // → 1: "/frontend-design:frontend-design"
        accept_selected(&s, &mut buf);
        assert_eq!(buf, "please run /frontend-design:frontend-design");
    }

    #[test]
    fn accept_selected_at_buffer_start_works() {
        let mut buf = "/impe".to_string();
        let s = suggest(&buf, &srcs());
        accept_selected(&s, &mut buf);
        assert_eq!(buf, "/impeccable");
    }

    #[test]
    fn accept_selected_with_no_candidates_is_noop() {
        let mut buf = "hello".to_string();
        let s = suggest(&buf, &srcs());
        accept_selected(&s, &mut buf);
        assert_eq!(buf, "hello");
    }

    #[test]
    fn select_helpers_are_noop_on_empty_state() {
        let mut s = SuggestionState::default();
        select_next(&mut s);
        select_prev(&mut s);
        assert_eq!(s.selected, 0);
    }
}
