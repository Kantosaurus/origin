// SPDX-License-Identifier: Apache-2.0
//! Block + inline markdown → styled rows. Pure.
//!
//! Wave 0 stub: locks `BlockKind`/`BlockStyle` and the `block_style` /
//! `render_inline` signatures. Wave 1 (Task B) fills the bodies, extending the
//! existing `mod.rs::render_md_line` to add italic / strike / links on top of
//! the kept bold / inline-code handling.

// The stub bodies look const-foldable now; Wave 1 fills them with real
// (non-const) layout logic, so they are deliberately left non-const.
#![allow(dead_code, clippy::missing_const_for_fn)] // Wave 1 fills this

use origin_tui::grid::{Attr, Grid};

use super::tokens::Tokens;
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

/// Classify one markdown line into its [`BlockStyle`] (ATX headings, bullets,
/// ordered items, quotes, rules, code fences).
#[must_use]
pub fn block_style(_line: &str, _tok: &Tokens) -> BlockStyle {
    // Wave 1 (Task B) fills this.
    BlockStyle::default()
}

/// Render the inline markdown of `text` into `grid` at `row`, starting at
/// `start_col`, clipping at `max_cols`. Handles bold / italic / strike / inline
/// code / links on top of the base `style`.
pub fn render_inline(
    _grid: &mut Grid,
    _row: u16,
    _text: &str,
    _max_cols: u16,
    _style: Style,
    _tok: &Tokens,
    _start_col: u16,
) {
    // Wave 1 (Task B) fills this.
}
