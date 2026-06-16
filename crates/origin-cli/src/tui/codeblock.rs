// SPDX-License-Identifier: Apache-2.0
//! Fenced code-block framing: language label + left rule + band background,
//! tinted via [`super::syntax`].
//!
//! Wave 0 stub: locks the `layout_code` signature. Wave 1 (Task B) fills the
//! body — left rule (`▎` in `accent_dim`), `band` bg, a dim language-label row,
//! and per-line tinting via `syntax::tint`.

// The stub body looks const-foldable now; Wave 1 fills it with real
// (non-const) layout logic, so it is deliberately left non-const.
#![allow(dead_code, clippy::missing_const_for_fn)] // Wave 1 fills this

use super::tokens::{RenderRow, Tokens};

/// Lay out a fenced code block (its lines, already fence-stripped) into framed,
/// tinted [`RenderRow`]s for `lang`, clipped to `width`.
#[must_use]
pub fn layout_code(_lines: &[&str], _lang: &str, _width: u16, _tok: &Tokens) -> Vec<RenderRow> {
    // Wave 1 (Task B) fills this.
    Vec::new()
}
