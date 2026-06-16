// SPDX-License-Identifier: Apache-2.0
//! Framed composer input field (`›` prompt glyph, soft-wrap cues) + hint line.
//!
//! Wave 0 stub: locks `EditorView` and the `draw_field`/`draw_hint`
//! signatures. Wave 1 (Task E) fills the bodies. `EditorView` field names are
//! derived from the live `Editor`/composer state the painter reads (wrapped
//! lines, caret, placeholder, internal scroll, row cap).

// The stub bodies look const-foldable now; Wave 1 fills them with real
// (non-const) paint logic, so they are deliberately left non-const.
#![allow(dead_code, clippy::missing_const_for_fn)] // Wave 1 fills this

use origin_tui::grid::Grid;

use super::tokens::{Region, Tokens};

/// A snapshot of the composer's editable state, ready to paint.
#[derive(Debug, Clone, Default)]
pub struct EditorView {
    /// The wrapped display lines of the input buffer.
    pub lines: Vec<String>,
    /// Caret row within `lines`.
    pub cursor_row: usize,
    /// Caret column (display cells) within its row.
    pub cursor_col: usize,
    /// Placeholder shown when the buffer is empty.
    pub placeholder: String,
    /// First visible line index when the field scrolls internally.
    pub scroll_top: usize,
    /// Maximum text rows the field renders before scrolling internally.
    pub max_rows: u16,
}

/// Paint the rounded composer frame + prompt glyph + soft-wrap cues into
/// `region`.
pub fn draw_field(_grid: &mut Grid, _region: Region, _ed: &EditorView, _tok: &Tokens) {
    // Wave 1 (Task E) fills this.
}

/// Paint the de-noised keybind hint line into `region`, dimming the in-flight
/// variant.
pub fn draw_hint(_grid: &mut Grid, _region: Region, _in_flight: bool, _tok: &Tokens) {
    // Wave 1 (Task E) fills this.
}
