// SPDX-License-Identifier: Apache-2.0
//! Transcript layout: copper spine gutter, per-turn role headers, turn rhythm,
//! hang-indent continuations.
//!
//! Wave 0 stub: locks the `layout_turn` signature + role-header helpers. The
//! body is filled in Wave 2 integration (it needs `mod.rs`'s scrollback model),
//! but the signature is locked here so the painter modules link against it.

// The stub bodies look const-foldable now; Wave 2 fills them with real
// (non-const) layout logic, so they are deliberately left non-const.
#![allow(dead_code, clippy::missing_const_for_fn)] // Wave 1/2 fills this

use super::tokens::{RenderRow, Tokens};
use super::ScrollLine;

/// Which role a transcript turn belongs to — drives the spine header glyph and
/// color (`you` warm / `◆ origin` copper).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    You,
    Origin,
}

/// Lay out one turn's finalized scrollback lines into spine-gutter
/// [`RenderRow`]s, with the role header, turn rhythm, and hang-indented
/// continuations.
#[must_use]
pub fn layout_turn(_lines: &[ScrollLine], _width: u16, _tok: &Tokens) -> Vec<RenderRow> {
    // Wave 2 integration fills this.
    Vec::new()
}

/// Build the role-header row for a turn (`◆ origin` / `you`), tied to the spine.
#[must_use]
pub fn role_header(_role: Role, _width: u16, _tok: &Tokens) -> RenderRow {
    // Wave 2 integration fills this.
    RenderRow::blank()
}
