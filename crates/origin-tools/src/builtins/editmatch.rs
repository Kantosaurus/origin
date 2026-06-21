// SPDX-License-Identifier: Apache-2.0
//! Whitespace-tolerant edit matching + near-miss diagnostics, shared by `Edit`
//! and `MultiEdit`.
//!
//! The caller always tries an EXACT substring match first. These helpers handle
//! the two failure cases that otherwise force the model to blind-retry:
//!  * zero exact matches → [`ws_tolerant_unique_range`] (a single
//!    indentation/trailing-whitespace-tolerant fallback) + [`closest_ws_line`]
//!    (a "closest line differs only in whitespace" hint), and
//!  * multiple exact matches → [`exact_match_lines`] (the 1-based line numbers
//!    so the model can add disambiguating context).
//!
//! The fallback NEVER guesses: it replaces a run only when EXACTLY one
//! whitespace-tolerant run matches.

use std::ops::Range;

/// Byte offsets of each line's content, excluding its trailing `\n`.
/// `(start, end)` where `text[start..end]` is the line without the newline.
fn line_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            spans.push((start, i));
            start = i + 1;
        }
    }
    // The final line (text after the last '\n', or the whole text if none).
    // When `text` ends in '\n' this is an empty trailing line — harmless, it
    // only ever matches an all-whitespace needle line.
    spans.push((start, text.len()));
    spans
}

/// The byte range of the UNIQUE whitespace-tolerant match of `needle` in
/// `text`, or `None` when zero or more than one run matches.
///
/// "Whitespace-tolerant" compares line-by-line with each line `.trim()`-ed,
/// absorbing leading-indent (tabs vs spaces, depth) and trailing-whitespace
/// drift while keeping interior content byte-exact. The returned range spans
/// from the start of the first matched line to the end of the last matched
/// line's content (newlines preserved), suitable for `String::replace_range`.
#[must_use]
pub fn ws_tolerant_unique_range(text: &str, needle: &str) -> Option<Range<usize>> {
    let needle_lines: Vec<&str> = needle.lines().map(str::trim).collect();
    if needle_lines.is_empty() {
        return None;
    }
    let spans = line_spans(text);
    let line_count = needle_lines.len();
    if line_count > spans.len() {
        return None;
    }
    let trimmed: Vec<&str> = spans.iter().map(|&(s, e)| text[s..e].trim()).collect();

    let mut found: Option<usize> = None;
    for start in 0..=spans.len() - line_count {
        if trimmed[start..start + line_count] == needle_lines[..] {
            if found.is_some() {
                return None; // more than one run ⇒ never guess
            }
            found = Some(start);
        }
    }
    let start = found?;
    Some(spans[start].0..spans[start + line_count - 1].1)
}

/// 1-based line of the closest whitespace-only near-miss.
///
/// The first line whose whitespace-collapsed form equals the (single-line)
/// `needle`'s — the "differs only in whitespace" hint. `None` for multi-line
/// needles or when there is no such near-miss.
#[must_use]
pub fn closest_ws_line(text: &str, needle: &str) -> Option<usize> {
    if needle.contains('\n') {
        return None;
    }
    let want = collapse_ws(needle);
    if want.is_empty() {
        return None;
    }
    text.lines()
        .position(|l| collapse_ws(l) == want)
        .map(|i| i + 1)
}

/// 1-based line numbers where `needle` occurs as an exact substring (the line
/// the match STARTS on). Used to disambiguate an ambiguous (>1 match) edit.
#[must_use]
pub fn exact_match_lines(text: &str, needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    text.match_indices(needle)
        .map(|(off, _)| text[..off].bytes().filter(|&b| b == b'\n').count() + 1)
        .collect()
}

/// Collapse all runs of ASCII/Unicode whitespace to a single space and trim —
/// so "differs only in whitespace" is detected regardless of indentation or
/// internal spacing. Used for the near-miss HINT only, never for replacement.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_fallback_matches_indentation_drift_uniquely() {
        // File uses 4-space indent; the needle uses a tab + trailing space.
        let text = "fn main() {\n    let x = 1;\n    let y = 2;\n}\n";
        let needle = "\tlet x = 1; ";
        let r = ws_tolerant_unique_range(text, needle).expect("unique ws match");
        assert_eq!(&text[r], "    let x = 1;", "range spans the real (indented) line");
    }

    #[test]
    fn ws_fallback_spans_multiple_lines() {
        let text = "a\n    foo\n    bar\nb\n";
        let needle = "foo\nbar"; // de-indented
        let r = ws_tolerant_unique_range(text, needle).expect("unique multi-line ws match");
        assert_eq!(&text[r], "    foo\n    bar");
    }

    #[test]
    fn ws_fallback_refuses_to_guess_when_ambiguous() {
        let text = "  x\nmid\n  x\n";
        assert!(
            ws_tolerant_unique_range(text, "x").is_none(),
            "two whitespace-tolerant runs ⇒ no guess"
        );
    }

    #[test]
    fn ws_fallback_none_when_content_differs() {
        let text = "    let x = 1;\n";
        assert!(ws_tolerant_unique_range(text, "let z = 9;").is_none());
    }

    #[test]
    fn closest_line_points_at_whitespace_only_difference() {
        let text = "fn main() {\n        let total = a + b;\n}\n";
        // Same tokens, only the leading indentation differs ⇒ a near-miss.
        assert_eq!(closest_ws_line(text, "let total = a + b;"), Some(2));
        // A genuine content difference (operator spacing) is NOT a near-miss.
        assert_eq!(closest_ws_line(text, "let total = a+b;"), None);
        assert_eq!(closest_ws_line(text, "no such line"), None);
    }

    #[test]
    fn exact_lines_reports_every_occurrence() {
        let text = "dup\nx\ndup\ny\ndup\n";
        assert_eq!(exact_match_lines(text, "dup"), vec![1, 3, 5]);
    }
}
