//! Emit `cursor-position + SGR + glyph` byte sequences for a damage-run set.
//!
//! Output is plain ANSI/VT100 — no terminfo dependency. Cursor reset (SGR 0)
//! is emitted between runs whose styles differ; within a run, style is set
//! once per style-change boundary (which is usually once).

use crate::damage::Run;
use crate::grid::{Attr, Grid};

/// Build the byte stream that, when written to a terminal already in sync with
/// `next` *before* these damage runs were applied, brings the display in line.
///
/// # Panics
/// Does not panic on well-formed input.
#[must_use]
pub fn emit(next: &Grid, runs: &[Run]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    if runs.is_empty() {
        return out;
    }
    for run in runs {
        // CUP: ESC [ row+1 ; col+1 H  (1-based)
        push_cup(&mut out, run.row, run.col);
        let mut current_style: Option<(u32, u32, u32)> = None; // (fg, bg, attr)
        for i in 0..run.len {
            let cell = next.get(run.row, run.col + i);
            let style = (cell.fg, cell.bg, cell.attr);
            if Some(style) != current_style {
                push_sgr(&mut out, cell.fg, cell.bg, Attr(cell.attr));
                current_style = Some(style);
            }
            push_glyph(&mut out, cell.glyph);
        }
        // Reset SGR after each run so subsequent unrelated writes start clean.
        out.extend_from_slice(b"\x1b[0m");
    }
    out
}

fn push_cup(out: &mut Vec<u8>, row: u16, col: u16) {
    use std::io::Write;
    let _ = write!(out, "\x1b[{};{}H", row + 1, col + 1);
}

fn push_sgr(out: &mut Vec<u8>, fg: u32, bg: u32, attr: Attr) {
    use std::io::Write;
    out.extend_from_slice(b"\x1b[0m");
    if attr.bits() & Attr::BOLD.bits() != 0 {
        out.extend_from_slice(b"\x1b[1");
        out.push(b'm');
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
    if fg != 0 {
        let (r, g, b) = unpack(fg);
        let _ = write!(out, "\x1b[38;2;{r};{g};{b}m");
    }
    if bg != 0 {
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

#[allow(clippy::cast_possible_truncation)] // intentional: extract 3 bytes from packed u32
const fn unpack(c: u32) -> (u8, u8, u8) {
    (
        ((c >> 16) & 0xFF) as u8,
        ((c >> 8) & 0xFF) as u8,
        (c & 0xFF) as u8,
    )
}
