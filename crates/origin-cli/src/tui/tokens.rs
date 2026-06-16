// SPDX-License-Identifier: Apache-2.0
//! Single source of truth for the TUI's colors, glyphs, and the
//! coordinate-free row primitives the painter modules emit.
//!
//! [`Tokens`] derives every named color role from a [`theme::Palette`] snapshot
//! so `/theme`, `NO_COLOR`, and the `HighContrast` variant all flow through one
//! place (Task F2). [`Region`] is a renderer-agnostic rectangle painters clip
//! to; [`RenderRow`]/[`RowSpan`] are owned styled rows a painter produces and
//! [`blit_row`] places into a [`Grid`] (Task F1). Decoupling painters from
//! absolute grid coordinates is what lets them be unit-tested and built in
//! parallel in Wave 1.

use origin_tui::grid::{Attr, Cell, Grid};

// ---------------------------------------------------------------------------
// Region / RenderRow / RowSpan + blit_row  (Task F1)
// ---------------------------------------------------------------------------

/// A renderer-agnostic rectangle. Painters receive a `Region` and clip their
/// output to it; the orchestrator (`mod.rs`) owns absolute placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub top: u16,
    pub left: u16,
    pub width: u16,
    pub height: u16,
}

impl Region {
    /// Construct a region from its origin and extent.
    #[must_use]
    pub const fn new(top: u16, left: u16, width: u16, height: u16) -> Self {
        Self {
            top,
            left,
            width,
            height,
        }
    }

    /// Last column inside the region (`left + width`, exclusive bound).
    #[must_use]
    pub const fn right(self) -> u16 {
        self.left.saturating_add(self.width)
    }

    /// Last row inside the region (`top + height`, exclusive bound).
    #[must_use]
    pub const fn bottom(self) -> u16 {
        self.top.saturating_add(self.height)
    }
}

/// One owned, styled visual row a painter emits.
///
/// `indent` shifts the whole row right when blitted (hang-indent / nesting);
/// `spans` are written left to right after the indent. Decouples painters from
/// absolute grid coordinates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderRow {
    pub spans: Vec<RowSpan>,
    pub indent: u16,
}

impl RenderRow {
    /// An empty row (blank line) with no indent.
    #[must_use]
    pub fn blank() -> Self {
        Self::default()
    }

    /// A single-span row at zero indent.
    #[must_use]
    pub fn one(span: RowSpan) -> Self {
        Self {
            spans: vec![span],
            indent: 0,
        }
    }
}

/// A run of text sharing one style, inside a [`RenderRow`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowSpan {
    pub text: String,
    pub fg: u32,
    pub bg: u32,
    pub attr: Attr,
}

impl RowSpan {
    /// Construct a styled span.
    #[must_use]
    pub fn new(text: impl Into<String>, fg: u32, bg: u32, attr: Attr) -> Self {
        Self {
            text: text.into(),
            fg,
            bg,
            attr,
        }
    }

    /// A plain-attribute span (most common case).
    #[must_use]
    pub fn plain(text: impl Into<String>, fg: u32, bg: u32) -> Self {
        Self::new(text, fg, bg, Attr::PLAIN)
    }
}

/// Display width of one char in terminal cells. Control chars are zero-width
/// (emitting one raw would move the terminal cursor and corrupt the frame).
///
/// Mirrors `mod.rs::char_cell_width`; kept here so painters/`blit_row` are
/// self-contained and unit-testable without the parent module.
#[must_use]
pub fn char_cell_width(c: char) -> u16 {
    use unicode_width::UnicodeWidthChar;
    if c.is_control() {
        return 0;
    }
    u16::try_from(UnicodeWidthChar::width(c).unwrap_or(1)).unwrap_or(1)
}

/// Blit one [`RenderRow`] into `grid` on absolute `row`, starting at
/// `base_col + r.indent`, clipping at `max_cols`.
///
/// Wide glyphs (width 2) write a [`Cell::continuation`] in the trailing column
/// and are skipped entirely if only one column remains before `max_cols`. When
/// a span carries a non-default `bg`, the background is filled to `max_cols`
/// after the row's text so the band reads as a solid block (matching the
/// existing `render_md_line` bg-fill behavior).
pub fn blit_row(grid: &mut Grid, row: u16, base_col: u16, max_cols: u16, r: &RenderRow) {
    let mut col = base_col.saturating_add(r.indent);
    // The bg to fill to `max_cols` after the row, taken from the last span that
    // carried a non-default background. `0` ⇒ no fill (transparent row).
    let mut trailing_bg: u32 = 0;
    for span in &r.spans {
        if span.bg != 0 {
            trailing_bg = span.bg;
        }
        for ch in span.text.chars() {
            if col >= max_cols {
                return_fill(grid, row, col, max_cols, trailing_bg);
                return;
            }
            let w = char_cell_width(ch);
            if w == 0 {
                // Zero-width control: skip without advancing (never emit raw).
                continue;
            }
            if col + w > max_cols {
                // A wide glyph that would straddle the clip boundary: stop and
                // fill the remaining single column with bg rather than splitting
                // the glyph across the edge.
                return_fill(grid, row, col, max_cols, trailing_bg);
                return;
            }
            grid.put(row, col, Cell::new(ch, span.fg, span.bg, span.attr));
            if w == 2 {
                grid.put(row, col + 1, Cell::continuation(span.bg));
            }
            col += w;
        }
    }
    return_fill(grid, row, col, max_cols, trailing_bg);
}

/// Fill `[col, max_cols)` on `row` with blank cells in `bg` (no-op when
/// `bg == 0`, i.e. a transparent row).
fn return_fill(grid: &mut Grid, row: u16, mut col: u16, max_cols: u16, bg: u32) {
    if bg == 0 {
        return;
    }
    while col < max_cols {
        grid.put(row, col, Cell::new(' ', 0, bg, Attr::PLAIN));
        col += 1;
    }
}

#[cfg(test)]
mod row_tests {
    use super::*;

    #[test]
    fn blit_row_clips_wide_glyph_at_boundary() {
        // "a你b": 'a'(1) '你'(2) 'b'(1). With max_cols = 2 the wide glyph would
        // straddle col 1..3, so it must be dropped (not split) and nothing past
        // the clip is written.
        let mut grid = Grid::new(4, 1);
        let row = RenderRow::one(RowSpan::plain("a\u{4f60}b", 0x00_FF_FF_FF, 0));
        blit_row(&mut grid, 0, 0, 2, &row);
        assert_eq!(grid.get(0, 0).glyph, 'a' as u32, "narrow glyph placed");
        // col 1 must NOT hold the wide glyph or its continuation — it stayed blank.
        assert_eq!(grid.get(0, 1).glyph, Cell::blank().glyph, "wide glyph clipped");
    }

    #[test]
    fn blit_row_writes_wide_continuation() {
        // '你' at col 0 occupies cols 0-1: a real glyph + a continuation cell.
        let mut grid = Grid::new(4, 1);
        let row = RenderRow::one(RowSpan::plain("\u{4f60}", 0x00_C0_C0_C0, 0));
        blit_row(&mut grid, 0, 0, 4, &row);
        assert_eq!(grid.get(0, 0).glyph, '\u{4f60}' as u32, "wide glyph placed");
        assert!(grid.get(0, 1).is_continuation(), "trailing half is a continuation");
    }

    #[test]
    fn blit_row_respects_indent_and_base_col() {
        let mut grid = Grid::new(8, 1);
        let row = RenderRow {
            spans: vec![RowSpan::plain("x", 0x00_FF_FF_FF, 0)],
            indent: 2,
        };
        blit_row(&mut grid, 0, 1, 8, &row);
        // base_col(1) + indent(2) = col 3.
        assert_eq!(grid.get(0, 3).glyph, 'x' as u32, "indent + base_col honored");
    }

    #[test]
    fn blit_row_fills_band_bg_to_max_cols() {
        // A span with a non-default bg fills its background to max_cols so a
        // code band reads as a solid block.
        let mut grid = Grid::new(6, 1);
        let bg = 0x00_16_13_11;
        let row = RenderRow::one(RowSpan::plain("ab", 0x00_FF_FF_FF, bg));
        blit_row(&mut grid, 0, 0, 6, &row);
        assert_eq!(grid.get(0, 2).bg, bg, "bg filled past the text");
        assert_eq!(grid.get(0, 5).bg, bg, "bg filled to max_cols");
    }
}
