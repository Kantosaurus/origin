// SPDX-License-Identifier: Apache-2.0
//! GFM task-list line rendering for the TUI scrollback.
//!
//! Pure, allocation-light helpers that recognise GitHub-flavored task-list
//! syntax (`- [ ] text`, `- [x] text`) and rewrite the `[ ]`/`[x]` marker as a
//! checkbox glyph. Kept free of any terminal/grid types so the recognition
//! rules are unit-testable in isolation; the TUI render path calls
//! [`render_gfm_task_line`] and falls through to normal rendering on `None`.

/// Unchecked checkbox glyph (`U+2610 BALLOT BOX`) substituted for `[ ]`.
pub const UNCHECKED_GLYPH: char = '\u{2610}';
/// Checked checkbox glyph (`U+2611 BALLOT BOX WITH CHECK`) substituted for
/// `[x]`/`[X]`.
pub const CHECKED_GLYPH: char = '\u{2611}';

/// Recognise a GFM task-list line and rewrite its marker as a checkbox glyph.
///
/// Returns `Some(rendered)` for a task-list line, preserving leading
/// whitespace and the text after the marker, with `- [ ]` replaced by the
/// unchecked glyph and `- [x]`/`- [X]` by the checked glyph. Returns `None`
/// for any line that is not a task-list item, so the caller renders it
/// unchanged.
///
/// Recognised shape: optional leading whitespace, a `-`, `*`, or `+` bullet, a
/// single space, `[`, a status char (space, `x`, or `X`), `]`, then a space and
/// the item text (or end of line). A bullet not followed by a space (`-[ ]`) is
/// rejected so only genuine GFM task lists are rewritten.
#[must_use]
pub fn render_gfm_task_line(line: &str) -> Option<String> {
    let indent_len = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(indent_len);

    let mut chars = rest.chars();
    let bullet = chars.next()?;
    if bullet != '-' && bullet != '*' && bullet != '+' {
        return None;
    }
    // The bullet must be followed by exactly one space, then the `[`.
    let after_bullet = chars.as_str();
    let marker = after_bullet.strip_prefix(' ')?;

    let (glyph, body) = parse_marker(marker)?;
    Some(format!("{indent}{glyph} {body}"))
}

/// Split a post-bullet remainder (`[ ] text` / `[x]` / `[X] text`) into the
/// checkbox glyph and the trailing item text.
///
/// Returns `None` when the bracket marker is malformed. The text after the
/// marker may be empty (a task line with no body) and is returned trimmed of a
/// single leading space.
fn parse_marker(marker: &str) -> Option<(char, &str)> {
    let inner = marker.strip_prefix('[')?;
    let mut mc = inner.chars();
    let state = mc.next()?;
    let glyph = match state {
        ' ' => UNCHECKED_GLYPH,
        'x' | 'X' => CHECKED_GLYPH,
        _ => return None,
    };
    let after_state = mc.as_str().strip_prefix(']')?;
    // Either end-of-line right after `]`, or a space then the body.
    if after_state.is_empty() {
        return Some((glyph, ""));
    }
    let body = after_state.strip_prefix(' ')?;
    Some((glyph, body))
}

#[cfg(test)]
mod tests {
    use super::{render_gfm_task_line, CHECKED_GLYPH, UNCHECKED_GLYPH};

    #[test]
    fn unchecked_becomes_unchecked_glyph() {
        let out = render_gfm_task_line("- [ ] buy milk").expect("task line");
        assert_eq!(out, format!("{UNCHECKED_GLYPH} buy milk"));
    }

    #[test]
    fn checked_lower_x_becomes_checked_glyph() {
        let out = render_gfm_task_line("- [x] ship it").expect("task line");
        assert_eq!(out, format!("{CHECKED_GLYPH} ship it"));
    }

    #[test]
    fn checked_upper_x_becomes_checked_glyph() {
        let out = render_gfm_task_line("- [X] ship it").expect("task line");
        assert_eq!(out, format!("{CHECKED_GLYPH} ship it"));
    }

    #[test]
    fn leading_whitespace_is_preserved() {
        let out = render_gfm_task_line("    - [ ] nested").expect("task line");
        assert_eq!(out, format!("    {UNCHECKED_GLYPH} nested"));
    }

    #[test]
    fn tab_indent_is_preserved() {
        let out = render_gfm_task_line("\t- [x] tabbed").expect("task line");
        assert_eq!(out, format!("\t{CHECKED_GLYPH} tabbed"));
    }

    #[test]
    fn star_and_plus_bullets_are_accepted() {
        assert_eq!(
            render_gfm_task_line("* [ ] star").expect("task line"),
            format!("{UNCHECKED_GLYPH} star")
        );
        assert_eq!(
            render_gfm_task_line("+ [x] plus").expect("task line"),
            format!("{CHECKED_GLYPH} plus")
        );
    }

    #[test]
    fn empty_body_task_line_is_accepted() {
        // `- [ ]` with no trailing text.
        let out = render_gfm_task_line("- [ ]").expect("task line");
        assert_eq!(out, format!("{UNCHECKED_GLYPH} "));
    }

    #[test]
    fn empty_body_with_trailing_space_is_accepted() {
        let out = render_gfm_task_line("- [x] ").expect("task line");
        assert_eq!(out, format!("{CHECKED_GLYPH} "));
    }

    #[test]
    fn bullet_without_space_is_not_a_task_line() {
        assert_eq!(render_gfm_task_line("-[ ] no space"), None);
    }

    #[test]
    fn plain_line_is_not_a_task_line() {
        assert_eq!(render_gfm_task_line("just some prose"), None);
    }

    #[test]
    fn plain_bullet_list_is_not_a_task_line() {
        assert_eq!(render_gfm_task_line("- a normal bullet"), None);
    }

    #[test]
    fn bad_marker_char_is_not_a_task_line() {
        // A non-space, non-x char inside the brackets is not a task list.
        assert_eq!(render_gfm_task_line("- [?] unknown"), None);
    }

    #[test]
    fn missing_close_bracket_is_not_a_task_line() {
        assert_eq!(render_gfm_task_line("- [ unterminated"), None);
    }

    #[test]
    fn empty_line_is_not_a_task_line() {
        assert_eq!(render_gfm_task_line(""), None);
    }
}
