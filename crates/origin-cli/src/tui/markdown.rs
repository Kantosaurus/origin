// SPDX-License-Identifier: Apache-2.0
//! Block + inline markdown → styled rows. Pure.
//!
//! `block_style` classifies one line (ATX headings, bullets, ordered items,
//! quotes, thematic rules, code fences) and resolves the colors/marker/indent
//! the painter applies. `render_inline` extends the base inline handling from
//! `mod.rs::render_md_line` — keeping **bold** and `code` — with `*italic*`,
//! `~~strike~~`, and `[txt](url)` links (link text in `accent` + underline, the
//! url hidden).

use origin_tui::grid::{Attr, Cell, Grid};

use super::tokens::{char_cell_width, Tokens};
use super::Style;

/// The block-level classification of one markdown line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum BlockKind {
    /// Ordinary paragraph text.
    #[default]
    Para,
    /// ATX heading, level 1–4.
    H(u8),
    /// Unordered list item at the given nesting depth.
    Bullet(u8),
    /// Ordered list item at the given nesting depth.
    Ordered(u8),
    /// Blockquote line (`>`).
    Quote,
    /// Thematic break (`---` / `***`).
    Rule,
    /// A fenced-code delimiter (open or close); `lang` is the info string.
    CodeFence { lang: String },
}

/// The fully-resolved styling for one markdown line: its block kind plus the
/// colors/attrs/marker/indent the painter applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockStyle {
    pub kind: BlockKind,
    pub fg: u32,
    pub bg: u32,
    pub attr: Attr,
    /// A leading marker glyph to draw (bullet, quote bar, number), if any.
    pub marker: Option<String>,
    /// Columns to indent the line body (list nesting / quote inset).
    pub indent: u16,
}

impl Default for BlockStyle {
    fn default() -> Self {
        Self {
            kind: BlockKind::Para,
            fg: 0,
            bg: 0,
            attr: Attr::PLAIN,
            marker: None,
            indent: 0,
        }
    }
}

/// One nesting level is two leading spaces (the conventional markdown indent
/// for a nested list item). Tabs count as one level each.
const INDENT_PER_LEVEL: u16 = 2;

/// Count the leading-whitespace nesting depth of `line` and return
/// `(depth, byte_offset_of_first_non_space)`. A tab counts as one full level;
/// every [`INDENT_PER_LEVEL`] spaces counts as one level.
fn leading_depth(line: &str) -> (u8, usize) {
    let mut spaces: u16 = 0;
    let mut levels: u16 = 0;
    let mut off = 0;
    for ch in line.chars() {
        match ch {
            ' ' => {
                spaces += 1;
                if spaces == INDENT_PER_LEVEL {
                    levels += 1;
                    spaces = 0;
                }
                off += 1;
            }
            '\t' => {
                levels += 1;
                spaces = 0;
                off += 1;
            }
            _ => break,
        }
    }
    (u8::try_from(levels).unwrap_or(u8::MAX), off)
}

/// True if `body` (already left-trimmed) is a thematic break: three or more of
/// the same `-`, `*`, or `_`, optionally separated by spaces, and nothing else.
fn is_thematic_break(body: &str) -> bool {
    let mut marker: Option<char> = None;
    let mut count = 0u32;
    for ch in body.chars() {
        match ch {
            '-' | '*' | '_' => {
                if let Some(m) = marker {
                    if m != ch {
                        return false;
                    }
                } else {
                    marker = Some(ch);
                }
                count += 1;
            }
            ' ' => {}
            _ => return false,
        }
    }
    count >= 3
}

/// Parse an ordered-list marker (`N.` or `N)`) at the start of `body`. Returns
/// the byte length of the marker-plus-trailing-space, if present.
fn ordered_marker_len(body: &str) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    // Cap the run so a long bare number isn't treated as a list (markdown
    // ordered markers are short).
    if i > 9 {
        return None;
    }
    if i < bytes.len() && (bytes[i] == b'.' || bytes[i] == b')') {
        let mut end = i + 1;
        // Require (or consume) a following space to be a real list item.
        if end < bytes.len() && bytes[end] == b' ' {
            end += 1;
            return Some(end);
        }
    }
    None
}

/// Classify one markdown line into its [`BlockStyle`] (ATX headings, bullets,
/// ordered items, quotes, rules, code fences).
#[must_use]
pub fn block_style(line: &str, tok: &Tokens) -> BlockStyle {
    let (depth, off) = leading_depth(line);
    let body = &line[off..];

    // Code fence (``` or ~~~) — open or close. The info string (lang) follows.
    if let Some(rest) = body.strip_prefix("```").or_else(|| body.strip_prefix("~~~")) {
        let lang = rest.trim().to_string();
        return BlockStyle {
            kind: BlockKind::CodeFence { lang },
            fg: tok.muted,
            bg: 0,
            attr: Attr::DIM,
            marker: None,
            indent: 0,
        };
    }

    // ATX heading: 1–4 leading `#` then a space (or end-of-line). Levels >4 fall
    // through to a paragraph.
    if body.starts_with('#') {
        let hashes = body.chars().take_while(|&c| c == '#').count();
        let after = &body[hashes..];
        if (1..=4).contains(&hashes) && (after.is_empty() || after.starts_with(' ')) {
            let level = u8::try_from(hashes).unwrap_or(4);
            let fg = match level {
                1 => tok.h1,
                2 => tok.h2,
                _ => tok.h3,
            };
            return BlockStyle {
                kind: BlockKind::H(level),
                fg,
                bg: 0,
                attr: Attr::BOLD,
                marker: None,
                indent: 0,
            };
        }
    }

    // Thematic break.
    if is_thematic_break(body) {
        return BlockStyle {
            kind: BlockKind::Rule,
            fg: tok.accent_dim,
            bg: 0,
            attr: Attr::DIM,
            marker: None,
            indent: 0,
        };
    }

    // Blockquote.
    if body.starts_with('>') {
        return BlockStyle {
            kind: BlockKind::Quote,
            fg: tok.muted,
            bg: 0,
            attr: Attr::ITALIC,
            marker: Some(super::tokens::glyph::QUOTE_BAR.to_string()),
            indent: depth.into(),
        };
    }

    // Unordered bullet: `-`, `*`, or `+` then a space.
    if let Some(first) = body.chars().next() {
        if matches!(first, '-' | '*' | '+') && body[first.len_utf8()..].starts_with(' ') {
            return BlockStyle {
                kind: BlockKind::Bullet(depth),
                fg: tok.body,
                bg: 0,
                attr: Attr::PLAIN,
                marker: Some(super::tokens::glyph::QUOTE_BAR.to_string()),
                indent: u16::from(depth) * INDENT_PER_LEVEL,
            };
        }
    }

    // Ordered list: `N.` / `N)` then a space.
    if let Some(mlen) = ordered_marker_len(body) {
        let marker = body[..mlen].trim_end().to_string();
        return BlockStyle {
            kind: BlockKind::Ordered(depth),
            fg: tok.body,
            bg: 0,
            attr: Attr::PLAIN,
            marker: Some(marker),
            indent: u16::from(depth) * INDENT_PER_LEVEL,
        };
    }

    // Default paragraph.
    BlockStyle {
        kind: BlockKind::Para,
        fg: tok.body,
        bg: 0,
        attr: Attr::PLAIN,
        marker: None,
        indent: 0,
    }
}

// ---------------------------------------------------------------------------
// Inline rendering
// ---------------------------------------------------------------------------

/// Scan `chars[start..]` for the next occurrence of the two-char marker
/// `(c1, c2)` and return its index. A single-char marker passes `c2 == '\0'`.
/// Mirrors `mod.rs::find_closing` but local so this module is self-contained and
/// does not depend on a non-`pub` parent item.
fn find_close(chars: &[char], start: usize, c1: char, c2: char) -> Option<usize> {
    let mut j = start;
    if c2 == '\0' {
        while j < chars.len() {
            if chars[j] == c1 {
                return Some(j);
            }
            j += 1;
        }
        return None;
    }
    while j + 1 < chars.len() {
        if chars[j] == c1 && chars[j + 1] == c2 {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// Write a run of `chars[from..to]` into `grid` at the running column, applying
/// `fg`/`bg`/`attr`. Stops at `max_cols`. Returns the new column.
#[allow(clippy::too_many_arguments)]
fn put_run(
    grid: &mut Grid,
    row: u16,
    chars: &[char],
    from: usize,
    to: usize,
    mut col: u16,
    max_cols: u16,
    fg: u32,
    bg: u32,
    attr: Attr,
) -> u16 {
    let mut i = from;
    while i < to && col < max_cols {
        let w = char_cell_width(chars[i]);
        if col + w > max_cols {
            break;
        }
        grid.put(row, col, Cell::new(chars[i], fg, bg, attr));
        if w == 2 {
            grid.put(row, col + 1, Cell::continuation(bg));
        }
        col += w;
        i += 1;
    }
    col
}

/// Render the inline markdown of `text` into `grid` at `row`, starting at
/// `start_col`, clipping at `max_cols`. Handles **bold**, `code`, *italic*,
/// ~~strike~~, and `[txt](url)` links on top of the base `style`.
///
/// Marker rules mirror the streaming-friendly behavior of the original
/// `render_md_line`: an *unclosed* span (still streaming, or wrapped to the next
/// visual line) styles to end-of-line with the marker hidden, rather than
/// leaking the literal marker fragment.
pub fn render_inline(
    grid: &mut Grid,
    row: u16,
    text: &str,
    max_cols: u16,
    style: Style,
    tok: &Tokens,
    start_col: u16,
) {
    let base_fg = style.fg;
    let bg = style.bg;
    let attr_plain = if style.bold { Attr::BOLD } else { Attr::PLAIN };
    let mut col: u16 = start_col;
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len && col < max_cols {
        let c = chars[i];
        let next = chars.get(i + 1).copied();

        // Bold: **…**
        if c == '*' && next == Some('*') {
            let close = find_close(&chars, i + 2, '*', '*');
            let (end, after) = close.map_or((len, len), |e| (e, e + 2));
            col = put_run(
                grid,
                row,
                &chars,
                i + 2,
                end,
                col,
                max_cols,
                tok.bright,
                bg,
                Attr::BOLD,
            );
            i = after;
            continue;
        }

        // Strike: ~~…~~
        if c == '~' && next == Some('~') {
            let close = find_close(&chars, i + 2, '~', '~');
            let (end, after) = close.map_or((len, len), |e| (e, e + 2));
            let attr = Attr(attr_plain.0 | Attr::STRIKE.0);
            col = put_run(grid, row, &chars, i + 2, end, col, max_cols, base_fg, bg, attr);
            i = after;
            continue;
        }

        // Link: [txt](url) — render txt in accent + underline; url hidden.
        if c == '[' {
            if let Some(rb) = find_close(&chars, i + 1, ']', '\0') {
                if chars.get(rb + 1) == Some(&'(') {
                    if let Some(rp) = find_close(&chars, rb + 2, ')', '\0') {
                        let attr = Attr(Attr::UNDERLINE.0);
                        col = put_run(grid, row, &chars, i + 1, rb, col, max_cols, tok.accent, bg, attr);
                        i = rp + 1; // skip ](url)
                        continue;
                    }
                }
            }
            // Not a well-formed link — fall through and render `[` literally.
        }

        // Inline code: `…` (single backtick, not part of a fence).
        if c == '`' && next != Some('`') {
            let close = find_close(&chars, i + 1, '`', '\0');
            let (end, after) = close.map_or((len, len), |e| (e, e + 1));
            col = put_run(
                grid,
                row,
                &chars,
                i + 1,
                end,
                col,
                max_cols,
                tok.code_fg,
                tok.code_bg,
                Attr::PLAIN,
            );
            i = after;
            continue;
        }

        // Italic: *…* (single asterisk; the **…** case is handled above) or _…_.
        if (c == '*' || c == '_') && next.is_some_and(|n| n != c) {
            let close = find_close(&chars, i + 1, c, '\0');
            let (end, after) = close.map_or((len, len), |e| (e, e + 1));
            let attr = Attr(attr_plain.0 | Attr::ITALIC.0);
            col = put_run(grid, row, &chars, i + 1, end, col, max_cols, base_fg, bg, attr);
            i = after;
            continue;
        }

        // Plain char.
        let w = char_cell_width(c);
        if col + w > max_cols {
            break;
        }
        grid.put(row, col, Cell::new(c, base_fg, bg, attr_plain));
        if w == 2 {
            grid.put(row, col + 1, Cell::continuation(bg));
        }
        col += w;
        i += 1;
    }

    // Fill the trailing background so a banded row reads as a solid block.
    if bg != 0 {
        while col < max_cols {
            grid.put(row, col, Cell::new(' ', 0, bg, Attr::PLAIN));
            col += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok() -> Tokens {
        Tokens::default_tokens()
    }

    // -- block_style -------------------------------------------------------

    #[test]
    fn headings_h1_through_h4() {
        let t = tok();
        for (line, lvl, fg) in [
            ("# Title", 1u8, t.h1),
            ("## Two", 2, t.h2),
            ("### Three", 3, t.h3),
            ("#### Four", 4, t.h3),
        ] {
            let s = block_style(line, &t);
            assert_eq!(s.kind, BlockKind::H(lvl), "{line}");
            assert_eq!(s.fg, fg, "{line} fg");
            assert_eq!(s.attr, Attr::BOLD, "{line} bold");
        }
    }

    #[test]
    fn five_hashes_is_a_paragraph_not_a_heading() {
        let s = block_style("##### too deep", &tok());
        assert_eq!(s.kind, BlockKind::Para);
    }

    #[test]
    fn bullets_nest_by_indent() {
        let t = tok();
        for (line, marker, depth) in [("- a", '-', 0u8), ("* b", '*', 0), ("+ c", '+', 0)] {
            let s = block_style(line, &t);
            assert_eq!(s.kind, BlockKind::Bullet(depth), "{line}");
            assert!(s.marker.is_some(), "{line} has a marker glyph");
            let _ = marker;
        }
        // Two-space indent ⇒ depth 1; four ⇒ depth 2.
        assert_eq!(block_style("  - nested", &t).kind, BlockKind::Bullet(1));
        assert_eq!(block_style("    - deeper", &t).kind, BlockKind::Bullet(2));
        assert_eq!(block_style("    - deeper", &t).indent, 4);
    }

    #[test]
    fn ordered_items() {
        let t = tok();
        let s = block_style("1. first", &t);
        assert_eq!(s.kind, BlockKind::Ordered(0));
        assert_eq!(s.marker.as_deref(), Some("1."));
        let s2 = block_style("12) twelfth", &t);
        assert_eq!(s2.kind, BlockKind::Ordered(0));
        assert_eq!(s2.marker.as_deref(), Some("12)"));
        // A bare number without a following space is a paragraph.
        assert_eq!(block_style("1.no-space", &t).kind, BlockKind::Para);
    }

    #[test]
    fn quote_has_bar_marker() {
        let s = block_style("> quoted", &tok());
        assert_eq!(s.kind, BlockKind::Quote);
        assert_eq!(
            s.marker.as_deref(),
            Some(super::super::tokens::glyph::QUOTE_BAR.to_string().as_str())
        );
    }

    #[test]
    fn thematic_rules() {
        let t = tok();
        for line in ["---", "***", "___", "- - -", "****"] {
            assert_eq!(block_style(line, &t).kind, BlockKind::Rule, "{line}");
        }
        // Mixed markers or fewer than three are not rules.
        assert_ne!(block_style("--", &t).kind, BlockKind::Rule);
        assert_ne!(block_style("-*-", &t).kind, BlockKind::Rule);
    }

    #[test]
    fn code_fence_open_and_close() {
        let t = tok();
        let open = block_style("```rust", &t);
        assert_eq!(open.kind, BlockKind::CodeFence { lang: "rust".into() });
        let close = block_style("```", &t);
        assert_eq!(close.kind, BlockKind::CodeFence { lang: String::new() });
        // Tilde fences too.
        assert_eq!(
            block_style("~~~py", &t).kind,
            BlockKind::CodeFence { lang: "py".into() }
        );
    }

    #[test]
    fn plain_paragraph_is_default_body() {
        let t = tok();
        let s = block_style("just some prose", &t);
        assert_eq!(s.kind, BlockKind::Para);
        assert_eq!(s.fg, t.body);
        assert_eq!(s.marker, None);
        assert_eq!(s.indent, 0);
    }

    // -- render_inline -----------------------------------------------------

    fn plain_style(fg: u32) -> Style {
        Style {
            fg,
            bg: 0,
            bold: false,
        }
    }

    /// Read the glyph at a cell as a `char`.
    fn glyph_at(grid: &Grid, row: u16, col: u16) -> char {
        char::from_u32(grid.get(row, col).glyph).unwrap_or('?')
    }

    #[test]
    fn italic_paints_italic_attr_and_hides_markers() {
        let t = tok();
        let mut grid = Grid::new(40, 1);
        render_inline(&mut grid, 0, "*hi*", 40, plain_style(0x00_C0_C0_C0), &t, 0);
        // Markers hidden: text starts at col 0 with 'h'.
        assert_eq!(glyph_at(&grid, 0, 0), 'h');
        assert_eq!(glyph_at(&grid, 0, 1), 'i');
        let a = grid.get(0, 0).attr;
        assert_ne!(a & Attr::ITALIC.0, 0, "italic flag set");
    }

    #[test]
    fn underscore_italic() {
        let t = tok();
        let mut grid = Grid::new(40, 1);
        render_inline(&mut grid, 0, "_yo_", 40, plain_style(0x00_C0_C0_C0), &t, 0);
        assert_eq!(glyph_at(&grid, 0, 0), 'y');
        assert_ne!(grid.get(0, 0).attr & Attr::ITALIC.0, 0);
    }

    #[test]
    fn strike_paints_strike_attr() {
        let t = tok();
        let mut grid = Grid::new(40, 1);
        render_inline(&mut grid, 0, "~~no~~", 40, plain_style(0x00_C0_C0_C0), &t, 0);
        assert_eq!(glyph_at(&grid, 0, 0), 'n');
        assert_eq!(glyph_at(&grid, 0, 1), 'o');
        assert_ne!(grid.get(0, 0).attr & Attr::STRIKE.0, 0, "strike flag set");
    }

    #[test]
    fn link_text_is_accent_underlined_and_url_hidden() {
        let t = tok();
        let mut grid = Grid::new(40, 1);
        render_inline(
            &mut grid,
            0,
            "[docs](https://x.io)",
            40,
            plain_style(0x00_C0_C0_C0),
            &t,
            0,
        );
        // Only "docs" is painted; the url and brackets are hidden.
        assert_eq!(glyph_at(&grid, 0, 0), 'd');
        assert_eq!(glyph_at(&grid, 0, 3), 's');
        // Next cell after the link text is blank (url not drawn).
        assert_eq!(grid.get(0, 4).glyph, Cell::blank().glyph, "url hidden");
        let cell = grid.get(0, 0);
        assert_eq!(cell.fg, t.accent, "link text uses accent");
        assert_ne!(cell.attr & Attr::UNDERLINE.0, 0, "link underlined");
    }

    #[test]
    fn bold_and_code_still_work() {
        let t = tok();
        let mut grid = Grid::new(40, 1);
        render_inline(&mut grid, 0, "**B**`c`", 40, plain_style(0x00_C0_C0_C0), &t, 0);
        // Bold 'B' at col 0.
        assert_eq!(glyph_at(&grid, 0, 0), 'B');
        assert_ne!(grid.get(0, 0).attr & Attr::BOLD.0, 0, "bold kept");
        assert_eq!(grid.get(0, 0).fg, t.bright, "bold uses bright fg");
        // Inline code 'c' at col 1 with code colors.
        assert_eq!(glyph_at(&grid, 0, 1), 'c');
        assert_eq!(grid.get(0, 1).fg, t.code_fg, "code fg");
        assert_eq!(grid.get(0, 1).bg, t.code_bg, "code bg");
    }

    #[test]
    fn unclosed_italic_styles_to_end_of_line() {
        // A dangling marker (still streaming) hides the marker and styles to EOL
        // rather than leaking the literal '*'.
        let t = tok();
        let mut grid = Grid::new(40, 1);
        render_inline(&mut grid, 0, "*streaming", 40, plain_style(0x00_C0_C0_C0), &t, 0);
        assert_eq!(glyph_at(&grid, 0, 0), 's', "marker hidden");
        assert_ne!(grid.get(0, 0).attr & Attr::ITALIC.0, 0, "styled italic to EOL");
    }

    #[test]
    fn malformed_link_renders_literally() {
        // `[txt]` with no `(url)` is not a link — the bracket renders literally.
        let t = tok();
        let mut grid = Grid::new(40, 1);
        render_inline(&mut grid, 0, "[x] y", 40, plain_style(0x00_C0_C0_C0), &t, 0);
        assert_eq!(glyph_at(&grid, 0, 0), '[');
    }

    #[test]
    fn clips_at_max_cols_without_overflow() {
        let t = tok();
        let mut grid = Grid::new(4, 1);
        // Far more text than fits; must not panic or write past max_cols(3).
        render_inline(&mut grid, 0, "abcdefgh", 3, plain_style(0x00_C0_C0_C0), &t, 0);
        assert_eq!(glyph_at(&grid, 0, 0), 'a');
        assert_eq!(glyph_at(&grid, 0, 2), 'c');
        // col 3 is the clip boundary — untouched.
        assert_eq!(grid.get(0, 3).glyph, Cell::blank().glyph);
    }
}
