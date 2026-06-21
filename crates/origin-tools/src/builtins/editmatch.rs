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

/// Replace the UNIQUE whitespace-tolerant match of `old` with `new`.
///
/// Re-indents `new` to the matched region so it doesn't land at column 0.
/// `None` when there is no unique ws-tolerant match (caller falls back to a
/// near-miss hint). The fallback edit path shared by `Edit`/`MultiEdit`.
#[must_use]
pub fn ws_unique_replace(text: &str, old: &str, new: &str) -> Option<String> {
    let range = ws_tolerant_unique_range(text, old)?;
    let reindented = reindent_replacement(new, old, &text[range.clone()]);
    let mut s = text.to_string();
    s.replace_range(range, &reindented);
    Some(s)
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

/// The leading-whitespace prefix of `line` (spaces/tabs before the first
/// non-whitespace char). Empty for a blank or unindented line.
fn leading_ws(line: &str) -> &str {
    let end = line.find(|c: char| c != ' ' && c != '\t').unwrap_or(line.len());
    &line[..end]
}

/// Leading whitespace of the first non-blank line of `s` (its indentation).
fn first_indent(s: &str) -> &str {
    s.lines().find(|l| !l.trim().is_empty()).map_or("", leading_ws)
}

/// Strip a pasted `Read` line-number gutter from `needle`, or `None` if absent.
///
/// Returns `Some` only when EVERY non-empty line is shaped `^ *\d+\t…` (cat -n:
/// right-aligned line number + tab), so a caller can retry the match against
/// real file text. `None` when any non-empty line lacks the gutter (ordinary
/// code — leave it alone). The ws-fallback can't fix this: the digits+tab are
/// real content drift, not whitespace.
#[must_use]
pub fn strip_read_gutter(needle: &str) -> Option<String> {
    let mut out = String::with_capacity(needle.len());
    let mut any = false;
    for (i, line) in needle.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if line.is_empty() {
            continue;
        }
        let stripped = strip_one_gutter(line)?;
        any = true;
        out.push_str(stripped);
    }
    // Preserve a trailing newline if the needle had one.
    if needle.ends_with('\n') {
        out.push('\n');
    }
    any.then_some(out)
}

/// Strip a single `^ *\d+\t` prefix from `line`, or `None` if it is absent.
fn strip_one_gutter(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches(' ');
    let digits_end = rest.find(|c: char| !c.is_ascii_digit())?;
    if digits_end == 0 || rest.as_bytes().get(digits_end) != Some(&b'\t') {
        return None;
    }
    Some(&rest[digits_end + 1..])
}

/// Re-indent `new_string` to the matched region's indentation, not the needle's.
///
/// Without this the ws-tolerant fallback splices `new_string` verbatim into a
/// more-indented range, producing column-0 lines (Python `IndentationError`,
/// broken blocks). `needle` and `matched_region` are the model's `old_string`
/// and the actual text being replaced. We reconcile only the simple cases (one
/// indent string is a prefix of the other); on a tab/space mismatch — or when
/// `new` is already at a different absolute indent — we leave it untouched.
#[must_use]
pub fn reindent_replacement(new_string: &str, needle: &str, matched_region: &str) -> String {
    let needle_indent = first_indent(needle);
    let file_indent = matched_region.lines().next().map_or("", leading_ws);
    if needle_indent == file_indent {
        return new_string.to_string();
    }
    // Only shift when the model wrote `new` in the NEEDLE's (de-indented)
    // coordinate system. If `new` is already at a different indentation, the
    // model chose that absolutely — trust it rather than double-indent.
    if first_indent(new_string) != needle_indent {
        return new_string.to_string();
    }
    if let Some(extra) = file_indent.strip_prefix(needle_indent) {
        // File is MORE indented than the needle → prepend the extra indent.
        if extra.is_empty() {
            return new_string.to_string();
        }
        return prefix_nonblank(new_string, extra);
    }
    if let Some(over) = needle_indent.strip_prefix(file_indent) {
        // Needle is MORE indented than the file → strip up to `over` per line.
        return strip_nonblank(new_string, over);
    }
    // ponytail: mixed tabs/spaces — neither prefixes the other; don't guess.
    new_string.to_string()
}

fn prefix_nonblank(s: &str, prefix: &str) -> String {
    join_lines_like(s, |line| {
        if line.trim().is_empty() {
            line.to_string()
        } else {
            format!("{prefix}{line}")
        }
    })
}

fn strip_nonblank(s: &str, over: &str) -> String {
    join_lines_like(s, |line| {
        line.strip_prefix(over).unwrap_or(line).to_string()
    })
}

/// Map `f` over each line of `s`, re-joining with `\n` and preserving a
/// trailing newline.
fn join_lines_like(s: &str, f: impl Fn(&str) -> String) -> String {
    let mut out = s.lines().map(f).collect::<Vec<_>>().join("\n");
    if s.ends_with('\n') {
        out.push('\n');
    }
    out
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

    #[test]
    fn gutter_strip_removes_read_line_numbers() {
        // Pasted straight from Read's `cat -n` output (right-aligned num + tab).
        let pasted = "  1\tfn main() {\n  2\t    let x = 1;\n";
        assert_eq!(
            strip_read_gutter(pasted).as_deref(),
            Some("fn main() {\n    let x = 1;\n")
        );
        // Ordinary code (no gutter) is left alone.
        assert_eq!(strip_read_gutter("fn main() {\n    let x = 1;"), None);
        // A single non-gutter line among gutter lines ⇒ not a gutter block.
        assert_eq!(strip_read_gutter("  1\tfoo\nbar"), None);
    }

    #[test]
    fn reindent_lands_new_string_at_file_indentation() {
        // Model de-indented the needle; the real region is 4-space indented.
        let region = "    foo();\n    bar();";
        let needle = "foo();\nbar();";
        let new = "baz();\nqux();";
        assert_eq!(
            reindent_replacement(new, needle, region),
            "    baz();\n    qux();",
            "replacement must inherit the file's indentation, not column 0"
        );
        // Equal indentation ⇒ unchanged.
        assert_eq!(reindent_replacement("x", "y", "y"), "x");
        // Blank lines in the replacement stay blank (no stray indent).
        assert_eq!(
            reindent_replacement("a\n\nb", "a\n\nb", "  a\n\n  b"),
            "  a\n\n  b"
        );
    }
}
