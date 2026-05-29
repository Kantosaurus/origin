// SPDX-License-Identifier: Apache-2.0
//! `StreamWidget` — reads `TokenEvent` payloads and lays graphemes into a Grid (N8.3).

use crate::grid::{Cell, Grid};
use crate::width::WidthCache;
use origin_stream::{TokenEvent, TokenKind};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub row: u16,
    pub col: u16,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug)]
pub struct StreamWidget {
    cursor_row: u16,
    cursor_col: u16,
    widths: WidthCache,
    viewport: Rect,
}

impl StreamWidget {
    #[must_use]
    pub fn new(viewport: Rect) -> Self {
        Self {
            cursor_row: viewport.row,
            cursor_col: viewport.col,
            widths: WidthCache::new(8 * 1024),
            viewport,
        }
    }

    pub fn reset_cursor(&mut self) {
        self.cursor_row = self.viewport.row;
        self.cursor_col = self.viewport.col;
    }

    pub fn apply(&mut self, event: &TokenEvent, grid: &mut Grid) {
        match event.kind() {
            TokenKind::TextDelta => self.write_text(event.payload(), grid),
            TokenKind::TurnEnd => self.newline(),
            _ => {}
        }
    }

    fn write_text(&mut self, bytes: &[u8], grid: &mut Grid) {
        let Ok(s) = std::str::from_utf8(bytes) else {
            return;
        };
        for g in s.graphemes(true) {
            let w = self.widths.width_of(g);
            let right_edge = self.viewport.col + self.viewport.cols;
            if self.cursor_col + u16::from(w) > right_edge {
                self.newline();
            }
            if let Some(ch) = g.chars().next() {
                grid.put(self.cursor_row, self.cursor_col, Cell::glyph(ch));
            }
            self.cursor_col += u16::from(w.max(1));
        }
    }

    fn newline(&mut self) {
        self.cursor_col = self.viewport.col;
        let bottom = self.viewport.row + self.viewport.rows.saturating_sub(1);
        if self.cursor_row < bottom {
            self.cursor_row += 1;
        }
    }
}
