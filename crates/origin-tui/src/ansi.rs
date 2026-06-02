// SPDX-License-Identifier: Apache-2.0
//! Emit `cursor-position + SGR + glyph` byte sequences for a damage-run set.

use std::sync::OnceLock;

use crate::damage::Run;
use crate::grid::Attr;
use crate::Grid;

/// Decide whether color SGR should be emitted, given the raw `NO_COLOR`
/// environment value.
///
/// Follows the de-facto `NO_COLOR` convention (used by clap/anstream): color
/// is suppressed when the variable is *present and non-empty*. An empty string
/// is treated as if unset so that `NO_COLOR=` does not silently disable color.
/// Note the convention is presence-based, not value-based — `NO_COLOR=0`
/// still disables color, by design.
const fn want_color(no_color: Option<&str>) -> bool {
    match no_color {
        Some(s) => s.is_empty(),
        None => true,
    }
}

/// Process-wide cached color decision, read once from `NO_COLOR`.
///
/// The environment is read a single time so every frame emits a consistent
/// byte stream and we avoid a `getenv` syscall on the render hot path.
/// Shared with `composer`, whose translated emit path mirrors this one.
pub(crate) fn want_color_cached() -> bool {
    static WANT_COLOR: OnceLock<bool> = OnceLock::new();
    *WANT_COLOR.get_or_init(|| want_color(std::env::var("NO_COLOR").ok().as_deref()))
}

#[must_use]
pub fn emit(next: &Grid, runs: &[Run]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    if runs.is_empty() {
        return out;
    }
    for run in runs {
        push_cup(&mut out, run.row, run.col);
        let mut current_style: Option<(u32, u32, u32)> = None;
        for i in 0..run.len {
            let cell = next.get(run.row, run.col + i);
            let style = (cell.fg, cell.bg, cell.attr);
            if Some(style) != current_style {
                push_sgr(&mut out, cell.fg, cell.bg, Attr(cell.attr));
                current_style = Some(style);
            }
            push_glyph(&mut out, cell.glyph);
        }
        out.extend_from_slice(b"\x1b[0m");
    }
    out
}

fn push_cup(out: &mut Vec<u8>, row: u16, col: u16) {
    use std::io::Write;
    let _ = write!(out, "\x1b[{};{}H", row + 1, col + 1);
}

fn push_sgr(out: &mut Vec<u8>, fg: u32, bg: u32, attr: Attr) {
    // Read the cached `NO_COLOR` decision once per style change; the env var is
    // resolved a single time process-wide (see `want_color_cached`).
    push_sgr_inner(out, fg, bg, attr, want_color_cached());
}

/// Emit the SGR sequence for a style, with the color decision passed in so the
/// behavior is unit-testable without touching the process environment.
///
/// Attribute sequences (bold/italic/underline/reverse/dim) are always emitted
/// so structure survives even with color off; only the 24-bit foreground and
/// background sequences are gated behind `want_color`.
fn push_sgr_inner(out: &mut Vec<u8>, fg: u32, bg: u32, attr: Attr, want_color: bool) {
    use std::io::Write;
    out.extend_from_slice(b"\x1b[0m");
    if attr.bits() & Attr::BOLD.bits() != 0 {
        out.extend_from_slice(b"\x1b[1m");
    }
    if attr.bits() & Attr::ITALIC.bits() != 0 {
        out.extend_from_slice(b"\x1b[3m");
    }
    if attr.bits() & Attr::UNDERLINE.bits() != 0 {
        out.extend_from_slice(b"\x1b[4m");
    }
    if attr.bits() & Attr::REVERSE.bits() != 0 {
        out.extend_from_slice(b"\x1b[7m");
    }
    if attr.bits() & Attr::DIM.bits() != 0 {
        out.extend_from_slice(b"\x1b[2m");
    }
    if want_color && fg != 0 {
        let (r, g, b) = unpack(fg);
        let _ = write!(out, "\x1b[38;2;{r};{g};{b}m");
    }
    if want_color && bg != 0 {
        let (r, g, b) = unpack(bg);
        let _ = write!(out, "\x1b[48;2;{r};{g};{b}m");
    }
}

fn push_glyph(out: &mut Vec<u8>, scalar: u32) {
    if let Some(ch) = char::from_u32(scalar) {
        let mut buf = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    }
}

const fn unpack(c: u32) -> (u8, u8, u8) {
    (
        ((c >> 16) & 0xFF) as u8,
        ((c >> 8) & 0xFF) as u8,
        (c & 0xFF) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::{push_sgr_inner, want_color};
    use crate::grid::Attr;

    #[test]
    fn want_color_honors_no_color_convention() {
        // Unset → color enabled (the default, byte-identical render path).
        assert!(want_color(None));
        // Present-but-empty is treated as unset by the NO_COLOR convention.
        assert!(want_color(Some("")));
        // Any non-empty value disables color — even "0", which the convention
        // deliberately does NOT special-case (presence is what matters).
        assert!(!want_color(Some("1")));
        assert!(!want_color(Some("0")));
    }

    #[test]
    fn disabled_color_keeps_attrs_drops_truecolor() {
        // A bold, foreground-colored cell. With color disabled the bold SGR
        // must survive (structure) but the 24-bit color SGR must be gone.
        let mut out: Vec<u8> = Vec::new();
        push_sgr_inner(&mut out, 0x00FF_8040, 0x0010_2030, Attr::BOLD, false);
        let s = String::from_utf8(out).expect("SGR bytes are valid UTF-8");
        assert!(s.contains("\x1b[1m"), "bold attribute should still emit");
        assert!(
            !s.contains("38;2"),
            "foreground truecolor must be skipped when color is disabled"
        );
        assert!(
            !s.contains("48;2"),
            "background truecolor must be skipped when color is disabled"
        );
    }
}
