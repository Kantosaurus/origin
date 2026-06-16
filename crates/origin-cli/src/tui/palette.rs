// SPDX-License-Identifier: Apache-2.0
//! Slash palette (described) + `@` file/agent mention picker popup.
//!
//! Wave 0 stub: locks `SlashItem`/`MentionItem`/`MentionKind` and the
//! `draw_slash`/`draw_mentions` signatures. Wave 1 (Task E) fills the bodies.
//! Field names are derived from the live `SuggestionState` the painter reads
//! (candidate names + their parallel descriptions, currently discarded).

// The stub bodies look const-foldable now; Wave 1 fills them with real
// (non-const) paint logic, so they are deliberately left non-const.
#![allow(dead_code, clippy::missing_const_for_fn)] // Wave 1 fills this

use origin_tui::grid::Grid;

use super::tokens::{Region, Tokens};

/// One described slash-command row in the palette.
#[derive(Debug, Clone, Default)]
pub struct SlashItem {
    /// The command/skill name (rendered in `accent`).
    pub name: String,
    /// Its description (rendered in `muted` — currently computed then discarded).
    pub desc: String,
}

/// One `@`-mention candidate row.
#[derive(Debug, Clone)]
pub struct MentionItem {
    /// The display text (path or agent name).
    pub display: String,
    /// Which kind of mention this is (drives a per-kind glyph).
    pub kind: MentionKind,
}

/// The kind of an `@`-mention candidate, driving its leading glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MentionKind {
    File,
    Dir,
    Agent,
}

/// Paint the described slash palette into `region`, highlighting row `sel`.
pub fn draw_slash(
    _grid: &mut Grid,
    _region: Region,
    _items: &[SlashItem],
    _sel: usize,
    _tok: &Tokens,
) {
    // Wave 1 (Task E) fills this.
}

/// Paint the `@` mention picker popup into `region`, highlighting row `sel`.
pub fn draw_mentions(
    _grid: &mut Grid,
    _region: Region,
    _items: &[MentionItem],
    _sel: usize,
    _tok: &Tokens,
) {
    // Wave 1 (Task E) fills this.
}
