// SPDX-License-Identifier: Apache-2.0
//! Framed composer input field (`›` prompt glyph, soft-wrap cues) + hint line.
//!
//! Wave 1 (Task E): paints the rounded composer frame and the de-noised keybind
//! hint. `EditorView` is a coordinate-free snapshot of the live composer state
//! the painter reads (wrapped lines, caret, placeholder, internal scroll, row
//! cap); `draw_field`/`draw_hint` write directly into the [`Grid`] within their
//! [`Region`], clipping to its bounds and reading every color from [`Tokens`].

use origin_tui::grid::{Attr, Cell, Grid};

use super::tokens::{char_cell_width, glyph, Region, Tokens};

/// Saturating `usize -> u16` (display dimensions never exceed `u16::MAX`).
fn u16c(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// A snapshot of the composer's editable state, ready to paint.
#[derive(Debug, Clone, Default)]
pub struct EditorView {
    /// The wrapped display lines of the input buffer.
    pub lines: Vec<String>,
    /// Caret row within `lines`. Part of the locked `EditorView` contract;
    /// the reverse-video caret is currently overlaid by the orchestrator
    /// (`mod.rs::draw_composer`) *after* the ghost-completion so it always wins
    /// the cell, so the painter itself does not read these. Reserved for the
    /// painter-owned caret in the INT-5 motion pass.
    #[allow(dead_code)]
    pub cursor_row: usize,
    /// Caret column (display cells) within its row. See [`Self::cursor_row`].
    #[allow(dead_code)]
    pub cursor_col: usize,
    /// Placeholder shown when the buffer is empty.
    pub placeholder: String,
    /// First visible line index when the field scrolls internally.
    pub scroll_top: usize,
    /// Maximum text rows the field renders before scrolling internally.
    pub max_rows: u16,
}

/// The rounded box-drawing corners + edges the composer frame is built from.
const TOP_LEFT: char = '\u{256D}'; // ╭
const TOP_RIGHT: char = '\u{256E}'; // ╮
const BOT_LEFT: char = '\u{2570}'; // ╰
const BOT_RIGHT: char = '\u{256F}'; // ╯
const VERT: char = '\u{2502}'; // │
const HORIZ: char = '\u{2500}'; // ─
/// Soft-wrap continuation cue shown in the gutter of wrapped rows (↳).
const WRAP_CUE: char = '\u{21B3}'; // ↳
/// "more above" indicator when the field scrolls past its first line (▴).
const MORE_ABOVE: char = '\u{25B4}'; // ▴

/// Put a single styled cell, ignoring out-of-bounds (`Grid::put` no-ops those).
fn cell(grid: &mut Grid, row: u16, col: u16, ch: char, fg: u32, bg: u32, attr: Attr) {
    grid.put(row, col, Cell::new(ch, fg, bg, attr));
}

/// Write a string left-to-right starting at `col`, clipping at `clip` (exclusive
/// column bound). Honors wide glyphs (writes a continuation cell + advances 2).
/// Returns the column just past the last glyph written.
#[allow(clippy::too_many_arguments)] // grid + position + text + clip + 3 style fields are all distinct
fn write_clip(
    grid: &mut Grid,
    row: u16,
    mut col: u16,
    text: &str,
    clip: u16,
    fg: u32,
    bg: u32,
    attr: Attr,
) -> u16 {
    for ch in text.chars() {
        if col >= clip {
            break;
        }
        let w = char_cell_width(ch);
        if w == 0 {
            continue;
        }
        if col + w > clip {
            break;
        }
        grid.put(row, col, Cell::new(ch, fg, bg, attr));
        if w == 2 {
            grid.put(row, col + 1, Cell::continuation(bg));
        }
        col += w;
    }
    col
}

/// Paint the rounded composer frame + prompt glyph + soft-wrap cues into
/// `region`.
///
/// Layout within `region`:
/// - top row: `╭──…──╮`
/// - content rows: `│ › <text|placeholder> … │`, the first content row carries
///   the `›` prompt glyph; wrapped continuation rows carry a dim `↳` cue in the
///   gutter; when `ed.scroll_top > 0` a `▴` "more above" marker replaces the
///   first content row's prompt gutter.
/// - bottom row: `╰──…──╯`
///
/// Everything is read from [`Tokens`] (frame in `accent_dim`, prompt in
/// `accent`, body text in `body`, placeholder/cues in `muted`).
// Long but linear frame paint (top/body/bottom + gutter cues); splitting it would
// just scatter the border math across helpers. Over the line cap only after rustfmt.
#[allow(clippy::too_many_lines)]
pub fn draw_field(grid: &mut Grid, region: Region, ed: &EditorView, tok: &Tokens) {
    if region.width < 2 || region.height < 2 {
        return;
    }
    let top = region.top;
    let left = region.left;
    let right = region.right(); // exclusive
    let last_col = right.saturating_sub(1); // the right border column
    let bottom = region.bottom().saturating_sub(1); // last row (border)

    // --- Frame: top + bottom borders -------------------------------------
    cell(grid, top, left, TOP_LEFT, tok.accent_dim, tok.raised, Attr::PLAIN);
    cell(
        grid,
        bottom,
        left,
        BOT_LEFT,
        tok.accent_dim,
        tok.raised,
        Attr::PLAIN,
    );
    let mut c = left + 1;
    while c < last_col {
        cell(grid, top, c, HORIZ, tok.accent_dim, tok.raised, Attr::PLAIN);
        cell(grid, bottom, c, HORIZ, tok.accent_dim, tok.raised, Attr::PLAIN);
        c += 1;
    }
    cell(
        grid,
        top,
        last_col,
        TOP_RIGHT,
        tok.accent_dim,
        tok.raised,
        Attr::PLAIN,
    );
    cell(
        grid,
        bottom,
        last_col,
        BOT_RIGHT,
        tok.accent_dim,
        tok.raised,
        Attr::PLAIN,
    );

    // --- Content rows between the borders --------------------------------
    let first_content = top + 1;
    let content_rows = bottom.saturating_sub(first_content); // exclusive of bottom
    if content_rows == 0 {
        return;
    }

    // The slice of wrapped lines visible after internal scroll, capped to the
    // available content rows (and to max_rows if the caller set one).
    let cap = if ed.max_rows == 0 {
        content_rows
    } else {
        content_rows.min(ed.max_rows)
    };
    let scroll = ed.scroll_top.min(ed.lines.len());
    let visible: Vec<&String> = ed.lines.iter().skip(scroll).take(cap as usize).collect();

    // gutter columns: │ <prompt> <text> … │
    //  left border @ left, gutter glyph @ left+1, a space @ left+2, text @ left+3
    let gutter_col = left + 1;
    let text_col = left + 3;
    let text_clip = last_col; // text stops before the right border

    let empty = ed.lines.iter().all(String::is_empty);

    for i in 0..content_rows {
        let row = first_content + i;
        // Paint the side borders + a raised-surface fill across the interior.
        cell(grid, row, left, VERT, tok.accent_dim, tok.raised, Attr::PLAIN);
        cell(grid, row, last_col, VERT, tok.accent_dim, tok.raised, Attr::PLAIN);
        let mut fc = left + 1;
        while fc < last_col {
            cell(grid, row, fc, ' ', 0, tok.raised, Attr::PLAIN);
            fc += 1;
        }

        // First content row owns the `›` prompt (or `▴` when scrolled).
        if i == 0 {
            let (g, fg) = if scroll > 0 {
                (MORE_ABOVE, tok.accent)
            } else {
                (glyph::PROMPT, tok.accent)
            };
            cell(grid, row, gutter_col, g, fg, tok.raised, Attr::BOLD);
        } else {
            // Continuation rows that actually carry wrapped text get the soft-wrap
            // cue; rows beyond the text stay a blank gutter.
            if (i as usize) < visible.len() {
                cell(
                    grid,
                    row,
                    gutter_col,
                    WRAP_CUE,
                    tok.muted,
                    tok.raised,
                    Attr::PLAIN,
                );
            }
        }

        // Body text (or the placeholder on the very first row when empty).
        if i == 0 && empty {
            write_clip(
                grid,
                row,
                text_col,
                &ed.placeholder,
                text_clip,
                tok.muted,
                tok.raised,
                Attr::ITALIC,
            );
        } else if let Some(line) = visible.get(i as usize) {
            write_clip(
                grid,
                row,
                text_col,
                line,
                text_clip,
                tok.body,
                tok.raised,
                Attr::PLAIN,
            );
        }
    }
}

/// The de-noised keybind hint segments, in display order. Each entry is a
/// `(key, label)` pair; keys render in `body` (bright), labels in `muted`.
///
/// `^c interrupt` is the trailing control: Ctrl+C interrupts the turn (the
/// reducer remaps it to Interrupt during a turn), so the hint stays accurate
/// whether or not a turn is in flight — the only in-flight difference is the
/// whole line dimming (see [`draw_hint`]).
const HINT_SEGMENTS: [(&str, &str); 5] = [
    ("\u{23CE}", "send"),            // ⏎ send
    ("\u{21E7}\u{23CE}", "newline"), // ⇧⏎ newline
    ("/", "skills"),
    ("@", "files"),
    ("^c", "interrupt"),
];

/// The middle-dot separator string between hint segments (` · `, 3 cells).
const SEP_STR: &str = " \u{00B7} "; // ·

/// Paint the de-noised, evenly-spaced keybind hint line into `region`, dimming
/// the whole line while a turn is in flight.
///
/// Renders `⏎ send · ⇧⏎ newline · / skills · @ files · ^c interrupt`, centered
/// within the region width, keys in `body`/`muted` and the in-flight variant
/// uniformly in `muted` with [`Attr::DIM`].
pub fn draw_hint(grid: &mut Grid, region: Region, in_flight: bool, tok: &Tokens) {
    if region.width == 0 || region.height == 0 {
        return;
    }
    let row = region.top;
    let clip = region.right();
    let segs = HINT_SEGMENTS;

    // Total display width = sum(key + space + label) + separators between segs.
    let seg_w = |key: &str, label: &str| char_str_width(key) + 1 + char_str_width(label);
    let mut total: u16 = 0;
    for (key, label) in &segs {
        total = total.saturating_add(seg_w(key, label));
    }
    let separator_w = char_str_width(SEP_STR); // " · "
    total = total.saturating_add(separator_w.saturating_mul(u16c(segs.len()).saturating_sub(1)));

    // Center within the region; left-align if it would overflow.
    let start = if total < region.width {
        region.left + (region.width - total) / 2
    } else {
        region.left
    };

    // When in flight, dim the whole line (keys + labels) to recede.
    let line_attr = if in_flight { Attr::DIM } else { Attr::PLAIN };
    let key_fg = if in_flight { tok.muted } else { tok.body };
    let label_fg = tok.muted;

    let mut col = start;
    for (idx, (key, label)) in segs.iter().enumerate() {
        if idx > 0 {
            // " · " separator between segments, in the recessive muted tone.
            col = write_clip(grid, row, col, SEP_STR, clip, tok.muted, 0, line_attr);
        }
        col = write_clip(grid, row, col, key, clip, key_fg, 0, line_attr);
        col = write_clip(grid, row, col, " ", clip, 0, 0, line_attr);
        col = write_clip(grid, row, col, label, clip, label_fg, 0, line_attr);
    }
}

/// Display width of a string in terminal cells (sum of per-char widths).
fn char_str_width(s: &str) -> u16 {
    s.chars().map(char_cell_width).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph_at(grid: &Grid, row: u16, col: u16) -> char {
        char::from_u32(grid.get(row, col).glyph).unwrap_or('\u{FFFD}')
    }

    /// Scan a row for the first cell whose glyph equals `target`.
    fn row_has(grid: &Grid, row: u16, target: char) -> bool {
        (0..grid.cols()).any(|c| glyph_at(grid, row, c) == target)
    }

    #[test]
    fn draw_field_paints_rounded_frame_corners() {
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(40, 5);
        let region = Region::new(0, 0, 40, 3);
        let ed = EditorView::default();
        draw_field(&mut grid, region, &ed, &tok);

        // Four rounded corners at the rectangle's extremes.
        assert_eq!(glyph_at(&grid, 0, 0), TOP_LEFT, "top-left ╭");
        assert_eq!(glyph_at(&grid, 0, 39), TOP_RIGHT, "top-right ╮");
        assert_eq!(glyph_at(&grid, 2, 0), BOT_LEFT, "bottom-left ╰");
        assert_eq!(glyph_at(&grid, 2, 39), BOT_RIGHT, "bottom-right ╯");
        // The top edge between corners is the horizontal rule.
        assert_eq!(glyph_at(&grid, 0, 10), HORIZ, "top edge ─");
        // The side borders are vertical bars.
        assert_eq!(glyph_at(&grid, 1, 0), VERT, "left border │");
        assert_eq!(glyph_at(&grid, 1, 39), VERT, "right border │");
    }

    #[test]
    fn draw_field_shows_prompt_glyph_on_first_content_row() {
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(40, 5);
        let region = Region::new(0, 0, 40, 3);
        let ed = EditorView::default();
        draw_field(&mut grid, region, &ed, &tok);
        // The `›` prompt glyph sits in the gutter of the first content row.
        assert_eq!(glyph_at(&grid, 1, 1), glyph::PROMPT, "› prompt glyph present");
    }

    #[test]
    fn draw_field_shows_placeholder_in_muted_when_empty() {
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(60, 5);
        let region = Region::new(0, 0, 60, 3);
        let ed = EditorView {
            placeholder: "ask origin to build".to_string(),
            ..EditorView::default()
        };
        draw_field(&mut grid, region, &ed, &tok);
        // Placeholder text begins at the text column (left + 3) in tok.muted.
        let first = grid.get(1, 3);
        assert_eq!(first.glyph, 'a' as u32, "placeholder first char rendered");
        assert_eq!(first.fg, tok.muted, "placeholder painted in muted");
        // And the placeholder substring is present on the row.
        assert!(row_has(&grid, 1, 'k'), "placeholder body rendered");
    }

    #[test]
    fn draw_field_hides_placeholder_when_non_empty() {
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(60, 5);
        let region = Region::new(0, 0, 60, 3);
        let ed = EditorView {
            lines: vec!["hello".to_string()],
            placeholder: "ask origin to build".to_string(),
            ..EditorView::default()
        };
        draw_field(&mut grid, region, &ed, &tok);
        // The buffer text shows in `body`, not the muted placeholder.
        let first = grid.get(1, 3);
        assert_eq!(first.glyph, 'h' as u32, "buffer text rendered");
        assert_eq!(first.fg, tok.body, "buffer text painted in body");
    }

    #[test]
    fn draw_field_shows_more_above_marker_when_scrolled() {
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(40, 6);
        let region = Region::new(0, 0, 40, 4);
        let ed = EditorView {
            lines: vec![
                "l0".to_string(),
                "l1".to_string(),
                "l2".to_string(),
                "l3".to_string(),
            ],
            scroll_top: 1,
            ..EditorView::default()
        };
        draw_field(&mut grid, region, &ed, &tok);
        // With scroll_top > 0 the first gutter shows the ▴ "more above" marker
        // in place of the prompt glyph.
        assert_eq!(glyph_at(&grid, 1, 1), MORE_ABOVE, "▴ more-above marker");
    }

    #[test]
    fn draw_field_shows_soft_wrap_cue_on_continuation_rows() {
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(40, 6);
        let region = Region::new(0, 0, 40, 4); // 2 content rows
        let ed = EditorView {
            lines: vec!["first".to_string(), "wrapped".to_string()],
            ..EditorView::default()
        };
        draw_field(&mut grid, region, &ed, &tok);
        // Second content row carries the ↳ soft-wrap continuation cue.
        assert_eq!(glyph_at(&grid, 2, 1), WRAP_CUE, "↳ soft-wrap cue");
    }

    #[test]
    fn draw_hint_renders_all_segments() {
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(80, 1);
        let region = Region::new(0, 0, 80, 1);
        draw_hint(&mut grid, region, false, &tok);
        // Each segment glyph/label is present on the row.
        assert!(row_has(&grid, 0, '\u{23CE}'), "⏎ send key");
        assert!(row_has(&grid, 0, '/'), "/ skills key");
        assert!(row_has(&grid, 0, '@'), "@ files key");
        assert!(row_has(&grid, 0, '\u{00B7}'), "· separator present");
        // The 'send' label is spelled out.
        assert!(row_has(&grid, 0, 's'), "send label rendered");
    }

    #[test]
    fn draw_hint_dims_in_flight_variant() {
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(80, 1);
        let region = Region::new(0, 0, 80, 1);
        draw_hint(&mut grid, region, true, &tok);
        // Find the `/` key cell and confirm it carries the DIM attr in-flight.
        let mut found = false;
        for c in 0..grid.cols() {
            let cell = grid.get(0, c);
            if cell.glyph == '/' as u32 {
                assert_eq!(cell.attr, Attr::DIM.bits(), "in-flight hint key is dimmed");
                assert_eq!(cell.fg, tok.muted, "in-flight key recedes to muted");
                found = true;
            }
        }
        assert!(found, "/ skills key present while in flight");
    }

    #[test]
    fn draw_field_no_panic_on_tiny_region() {
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(4, 4);
        // Degenerate regions must be no-ops, not panics.
        draw_field(&mut grid, Region::new(0, 0, 1, 1), &EditorView::default(), &tok);
        draw_field(&mut grid, Region::new(0, 0, 0, 0), &EditorView::default(), &tok);
        draw_field(&mut grid, Region::new(0, 0, 3, 2), &EditorView::default(), &tok);
    }
}
