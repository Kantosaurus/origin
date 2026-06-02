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

/// Height of the suggestion popup in rows.
///
/// The full ranked candidate list can be longer than this; the TUI renders a
/// scrolling window of `MAX_VISIBLE` rows over it (see [`scroll_offset`]), so
/// every match stays reachable by arrowing.
pub const MAX_VISIBLE: usize = 6;

#[derive(Debug, Clone, Default)]
pub struct SuggestionState {
    /// Wrapped candidate strings (already includes the leading `/`, `/-`,
    /// or `{workflow:` syntax so they can be rendered verbatim).
    pub candidates: Vec<String>,
    /// Short descriptions parallel to [`candidates`](Self::candidates) — same
    /// length and order. Empty strings for candidates with no known
    /// description. Additive: a later wave renders these next to each row;
    /// today's consumers ignore the field.
    pub descriptions: Vec<String>,
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
        // Workflows carry no descriptions; pass an empty parallel list.
        return match_candidates(
            token,
            partial,
            prefix_len,
            &sources.workflows,
            &[],
            |full| format!("{{workflow:{full}}}"),
        );
    }
    // Skill shapes (`/-` deactivate, `/` activate) match the combined
    // verb+skill candidate list with descriptions kept index-aligned.
    let names = sources.skill_candidates();
    let descs = sources.skill_candidate_descriptions();
    if let Some(partial) = token.strip_prefix("/-") {
        return match_candidates(token, partial, prefix_len, &names, &descs, |full| {
            format!("/-{full}")
        });
    }
    if let Some(partial) = token.strip_prefix('/') {
        return match_candidates(token, partial, prefix_len, &names, &descs, |full| {
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

/// Relevance buckets for a candidate against the typed `partial`, ordered
/// best-first. Sorted ascending so a smaller discriminant ranks higher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    /// Candidate begins with `partial` (case-insensitive).
    ExactPrefix,
    /// `partial` starts a word inside the candidate (after a non-alphanumeric
    /// boundary such as `-`, `:`, `_`), but not at the very start.
    WordBoundary,
    /// `partial` appears somewhere inside the candidate (case-insensitive).
    Substring,
}

/// Compute the [`Rank`] of `name` against the lowercased `needle`, or `None`
/// when `needle` doesn't appear at all. An empty `needle` matches every
/// candidate as an `ExactPrefix` (bare `/` lists everything).
fn rank_of(name: &str, needle: &str) -> Option<Rank> {
    if needle.is_empty() {
        return Some(Rank::ExactPrefix);
    }
    let lower = name.to_lowercase();
    let pos = lower.find(needle)?;
    if pos == 0 {
        return Some(Rank::ExactPrefix);
    }
    // Word-boundary when the char immediately before the match is a
    // recognized separator. Indexing into `lower` is byte-safe because
    // ASCII separators are single-byte and `find` returns a char boundary.
    let at_word_boundary = lower[..pos]
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_alphanumeric());
    if at_word_boundary {
        Some(Rank::WordBoundary)
    } else {
        Some(Rank::Substring)
    }
}

fn match_candidates(
    token: &str,
    partial: &str,
    prefix_len: usize,
    names: &[String],
    descriptions: &[String],
    wrap: impl Fn(&str) -> String,
) -> SuggestionState {
    let needle = partial.to_lowercase();
    // Rank every candidate, keeping its original index so we can recover the
    // parallel description and preserve stable order within a rank.
    let mut ranked: Vec<(Rank, usize)> = names
        .iter()
        .enumerate()
        .filter_map(|(i, name)| rank_of(name, &needle).map(|r| (r, i)))
        .collect();
    // Stable sort by rank only; equal ranks keep their source order so the
    // ordering is deterministic ("stable order within a rank"). The full list
    // is kept — the popup scrolls a `MAX_VISIBLE` window over it rather than
    // truncating, so a low-ranked match is still reachable by arrowing.
    ranked.sort_by_key(|(rank, _)| *rank);

    let candidates: Vec<String> = ranked.iter().map(|&(_, i)| wrap(&names[i])).collect();
    let candidate_descs: Vec<String> = ranked
        .iter()
        .map(|&(_, i)| descriptions.get(i).cloned().unwrap_or_default())
        .collect();

    // Ghost text is a TRUE case-sensitive PREFIX of the trailing token so it
    // never proposes characters the user didn't actually type. Only when
    // there's exactly one match AND it extends the token verbatim.
    let ghost = if candidates.len() == 1
        && candidates[0].starts_with(token)
        && candidates[0].len() > token.len()
    {
        candidates[0][token.len()..].to_string()
    } else {
        String::new()
    };

    SuggestionState {
        candidates,
        descriptions: candidate_descs,
        ghost,
        selected: 0,
        prefix_len,
    }
}

/// Top index of the visible window so the `selected` candidate is always shown
/// within a window of [`MAX_VISIBLE`] rows.
///
/// Given a candidate-list length `len` and the `selected` index, the popup
/// shows `candidates[offset .. offset + MAX_VISIBLE]`. When the full list fits
/// (`len <= MAX_VISIBLE`) the window is anchored at `0`; otherwise it scrolls
/// just enough to keep `selected` on the bottom edge, clamped to the last page.
#[must_use]
pub fn scroll_offset(len: usize, selected: usize) -> usize {
    if len <= MAX_VISIBLE {
        return 0;
    }
    let max_offset = len - MAX_VISIBLE;
    // Anchor the window so `selected` sits at its bottom edge once we've
    // scrolled past the first page, then clamp to the final page.
    selected.saturating_sub(MAX_VISIBLE - 1).min(max_offset)
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
            ..Default::default()
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

    #[test]
    fn scroll_offset_short_list_does_not_scroll() {
        assert_eq!(scroll_offset(0, 0), 0);
        assert_eq!(scroll_offset(MAX_VISIBLE, 0), 0);
        assert_eq!(scroll_offset(MAX_VISIBLE, MAX_VISIBLE - 1), 0);
        assert_eq!(scroll_offset(3, 2), 0);
    }

    #[test]
    fn scroll_offset_keeps_selection_visible() {
        let len = MAX_VISIBLE + 4;
        // First page: every selection in [0, MAX_VISIBLE) needs no scroll.
        for sel in 0..MAX_VISIBLE {
            assert_eq!(scroll_offset(len, sel), 0, "sel={sel}");
        }
        // Past the first page the window tracks the selection.
        assert_eq!(scroll_offset(len, MAX_VISIBLE), 1);
        assert_eq!(scroll_offset(len, MAX_VISIBLE + 1), 2);
        // Last item anchors the final page.
        assert_eq!(scroll_offset(len, len - 1), len - MAX_VISIBLE);
        // Selected is always within [offset, offset + MAX_VISIBLE).
        for sel in 0..len {
            let off = scroll_offset(len, sel);
            assert!(sel >= off && sel < off + MAX_VISIBLE, "sel={sel} off={off}");
        }
    }

    /// The full ranked list is returned (not truncated to `MAX_VISIBLE`) so the
    /// scrolling popup can reach every match by arrowing.
    #[test]
    fn returns_all_matches_for_scrolling() {
        let many = CompletionSources {
            skills: (0..MAX_VISIBLE + 3).map(|i| format!("widget-{i}")).collect(),
            ..Default::default()
        };
        let s = suggest("/widget", &many);
        assert_eq!(
            s.candidates.len(),
            MAX_VISIBLE + 3,
            "all matching candidates must be kept for scrolling, got {:?}",
            s.candidates
        );
    }

    // -- New discovery behavior: built-in verbs, case-insensitive substring,
    //    ranking, description carry-through, case-sensitive ghost. -----------

    fn srcs_with_verbs() -> CompletionSources {
        CompletionSources {
            skills: vec![
                "systematic-debugging".into(),
                "impeccable".into(),
            ],
            skill_descriptions: vec!["debug methodically".into(), "polish UI".into()],
            verbs: vec!["effort".into(), "clear".into()],
            verb_descriptions: vec!["set reasoning effort".into(), "clear the chat".into()],
            workflows: vec![],
        }
    }

    /// Typing `/eff` surfaces the `effort` built-in verb candidate.
    #[test]
    fn builtin_verb_effort_is_suggested() {
        let s = suggest("/eff", &srcs_with_verbs());
        assert!(
            s.candidates.iter().any(|c| c == "/effort"),
            "expected `/effort` among candidates, got {:?}",
            s.candidates
        );
    }

    /// A mixed-case query matches a skill by case-insensitive substring:
    /// `DEBUG` matches `systematic-debugging`.
    #[test]
    fn mixed_case_substring_matches_skill() {
        let s = suggest("/DEBUG", &srcs_with_verbs());
        assert!(
            s.candidates.iter().any(|c| c == "/systematic-debugging"),
            "expected `/systematic-debugging` for query /DEBUG, got {:?}",
            s.candidates
        );
    }

    /// Ranking: an exact-prefix match sorts before a mid-word substring
    /// match. Query `de` is an exact prefix of `debug-helper` and a mid-word
    /// substring of `wonder-tool` — the prefix candidate must come first.
    #[test]
    fn exact_prefix_outranks_midword_substring() {
        let sources = CompletionSources {
            // Source order deliberately puts the substring match FIRST so a
            // plain stable sort by source order would fail; ranking must lift
            // the exact-prefix candidate above it.
            skills: vec!["wonder-tool".into(), "debug-helper".into()],
            ..Default::default()
        };
        let s = suggest("/de", &sources);
        assert_eq!(s.candidates[0], "/debug-helper");
        assert_eq!(s.candidates[1], "/wonder-tool");
    }

    /// Word-boundary matches outrank plain mid-word substring matches.
    /// Query `bug`: `systematic-debugging` has `bug` mid-word (substring),
    /// `auto-bug-finder` has `bug` right after a `-` (word boundary).
    #[test]
    fn word_boundary_outranks_plain_substring() {
        let sources = CompletionSources {
            skills: vec!["systematic-debugging".into(), "auto-bug-finder".into()],
            ..Default::default()
        };
        let s = suggest("/bug", &sources);
        assert_eq!(s.candidates[0], "/auto-bug-finder");
        assert_eq!(s.candidates[1], "/systematic-debugging");
    }

    /// Ghost text stays a TRUE case-sensitive prefix: a lowercase query does
    /// NOT ghost-complete an uppercase candidate even though it matches by
    /// case-insensitive substring.
    #[test]
    fn ghost_stays_case_sensitive_prefix() {
        let sources = CompletionSources {
            skills: vec!["Impeccable".into()],
            ..Default::default()
        };
        let s = suggest("/imp", &sources);
        // It still MATCHES (case-insensitive) and shows as a candidate...
        assert_eq!(s.candidates, vec!["/Impeccable".to_string()]);
        // ...but no ghost, because `/imp` is not a case-sensitive prefix of
        // `/Impeccable` (would propose chars the user didn't type).
        assert!(
            s.ghost.is_empty(),
            "expected empty ghost for case-mismatched prefix, got {:?}",
            s.ghost
        );
    }

    /// A case-sensitive prefix still ghost-completes as before.
    #[test]
    fn ghost_still_fires_for_case_sensitive_prefix() {
        let sources = CompletionSources {
            skills: vec!["impeccable".into()],
            ..Default::default()
        };
        let s = suggest("/imp", &sources);
        assert_eq!(s.ghost, "eccable");
    }

    /// Descriptions are carried through, index-aligned with candidates.
    #[test]
    fn descriptions_align_with_candidates() {
        let s = suggest("/eff", &srcs_with_verbs());
        assert_eq!(s.candidates.len(), s.descriptions.len());
        let pair = s
            .candidates
            .iter()
            .zip(s.descriptions.iter())
            .find(|(c, _)| *c == "/effort");
        assert_eq!(pair, Some((&"/effort".to_string(), &"set reasoning effort".to_string())));
    }
}
