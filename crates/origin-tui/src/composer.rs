// SPDX-License-Identifier: Apache-2.0
//! `Composer` — a three-pane layout (main, side, prompt) backed by six
//! `Grid`s: one live and one scratch per pane, enabling SIMD damage diff via
//! `damage::diff` + `ansi::emit` on each frame.

use std::io::Write as _;

use crate::damage::{self, Run};
use crate::grid::{Attr, Cell};
use crate::Grid;

/// NUL-glyph sentinel cell used to initialize scratch grids.
/// Distinct from `Cell::blank()` (glyph `' '`) so the first frame after
/// construction or resize triggers a full repaint.
const SCRATCH_SENTINEL: Cell = Cell {
    glyph: 0,
    fg: 0,
    bg: 0,
    attr: 0,
};

/// Three-pane terminal composer.
///
/// Manages a main pane, a (optionally hidden) side panel, and a prompt bar,
/// each with a corresponding scratch grid used for damage diffing.
#[derive(Debug)]
pub struct Composer {
    /// Total columns of the terminal.
    cols: u16,
    /// Total rows of the terminal.
    rows: u16,
    /// Column width of the side panel (0 when hidden).
    side_cols: u16,

    // Live grids written by callers.
    main: Grid,
    side: Grid,
    prompt: Grid,

    // Shadow copies for damage diffing.
    scratch_main: Grid,
    scratch_side: Grid,
    scratch_prompt: Grid,

    side_visible: bool,
}

// ─── layout helpers ──────────────────────────────────────────────────────────

pub const PROMPT_ROWS: u16 = 1;

fn compute_side_cols(cols: u16, visible: bool) -> u16 {
    if visible {
        (cols / 3).clamp(20, 40)
    } else {
        0
    }
}

/// Allocate a new `Grid` of the requested size, copying any cells from `old`
/// that still fit in the new bounds.  Cells outside the new bounds are
/// silently dropped; cells that are new (no old counterpart) are blank.
fn resize_clipped(old: &Grid, new_cols: u16, new_rows: u16) -> Grid {
    let mut g = Grid::new(new_cols, new_rows);
    let copy_cols = old.cols().min(new_cols);
    let copy_rows = old.rows().min(new_rows);
    for r in 0..copy_rows {
        for c in 0..copy_cols {
            let cell = old.get(r, c);
            g.put(r, c, cell);
        }
    }
    g
}

// ─── translated ANSI emission ────────────────────────────────────────────────

/// Emit ANSI byte sequences for `runs`, translating pane-relative row/col
/// coordinates to absolute screen coordinates by adding `row_offset` /
/// `col_offset`.
fn emit_translated(grid: &Grid, runs: &[Run], row_offset: u16, col_offset: u16) -> Vec<u8> {
    if runs.is_empty() {
        return Vec::new();
    }
    // Translate runs to absolute screen coordinates.
    let translated: Vec<Run> = runs
        .iter()
        .map(|r| Run {
            row: r.row + row_offset,
            col: r.col + col_offset,
            len: r.len,
        })
        .collect();
    // We need to render cells from the pane grid but emit CUP using absolute
    // coords.  `ansi::emit` reads cell data from `grid` using `run.row` /
    // `run.col`, so we must re-implement the hot path here to keep pane-
    // relative cell lookup while using absolute CUP positions.
    let mut out: Vec<u8> = Vec::new();
    for (orig, abs) in runs.iter().zip(translated.iter()) {
        // CUP with absolute position.
        let _ = write!(out, "\x1b[{};{}H", abs.row + 1, abs.col + 1);
        let mut current_style: Option<(u32, u32, u32)> = None;
        for i in 0..orig.len {
            // Cell lookup uses pane-relative coords.
            let cell = grid.get(orig.row, orig.col + i);
            // The continuation half of a wide glyph emits nothing: the wide glyph
            // already advanced the terminal cursor by two columns, so writing
            // here would shift the rest of the row right by one.
            if cell.is_continuation() {
                continue;
            }
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

fn push_sgr(out: &mut Vec<u8>, fg: u32, bg: u32, attr: Attr) {
    // Honor `NO_COLOR` via the same cached decision the `ansi` emit path uses,
    // so both render paths stay byte-consistent.
    push_sgr_inner(out, fg, bg, attr, crate::ansi::want_color_cached());
}

/// Emit the SGR sequence for a style, with the color decision passed in so the
/// behavior is unit-testable without touching the process environment.
///
/// Attribute sequences (bold/italic/underline/reverse/dim) are always emitted
/// so structure survives even with color off; only the 24-bit foreground and
/// background sequences are gated behind `want_color`.
fn push_sgr_inner(out: &mut Vec<u8>, fg: u32, bg: u32, attr: Attr, want_color: bool) {
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

// ─── Composer ────────────────────────────────────────────────────────────────

impl Composer {
    /// Create a new `Composer` for a terminal of `cols × rows`.
    ///
    /// Both live and scratch grids start filled with a NUL-glyph sentinel
    /// (distinct from `Cell::blank()` whose glyph is `' '`).  This means:
    ///
    /// * An immediate `frame()` call on a freshly constructed `Composer`
    ///   returns empty bytes (live == scratch → no damage).
    /// * The first `draw()` call clears the live panes to `Cell::blank()`
    ///   (glyph `' '`), which differs from the NUL-glyph scratch, so the
    ///   subsequent `frame()` emits **every** live cell — including space
    ///   characters that would otherwise look identical to blank scratch.
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        // The side panel is hidden by default — the main pane uses the
        // full terminal width so the input card stays centered. Callers
        // that want a sidebar can flip `side_visible` via `resize`.
        let side_visible = false;
        let side_cols = compute_side_cols(cols, side_visible);
        let pane_rows = rows.saturating_sub(PROMPT_ROWS);
        let main_cols = cols.saturating_sub(side_cols);

        Self {
            cols,
            rows,
            side_cols,
            main: Grid::new_filled(main_cols, pane_rows, SCRATCH_SENTINEL),
            side: Grid::new_filled(side_cols.max(1), pane_rows, SCRATCH_SENTINEL),
            prompt: Grid::new_filled(cols, PROMPT_ROWS, SCRATCH_SENTINEL),
            scratch_main: Grid::new_filled(main_cols, pane_rows, SCRATCH_SENTINEL),
            scratch_side: Grid::new_filled(side_cols.max(1), pane_rows, SCRATCH_SENTINEL),
            scratch_prompt: Grid::new_filled(cols, PROMPT_ROWS, SCRATCH_SENTINEL),
            side_visible,
        }
    }

    /// Resize all panes, clipping existing content (no rewrap).
    pub fn resize(&mut self, cols: u16, rows: u16, side_visible: bool) {
        self.cols = cols;
        self.rows = rows;
        self.side_visible = side_visible;

        let side_cols = compute_side_cols(cols, side_visible);
        self.side_cols = side_cols;

        let pane_rows = rows.saturating_sub(PROMPT_ROWS);
        let main_cols = cols.saturating_sub(side_cols);

        // Preserve existing content in the live grids (clip, not rewrap).
        self.main = resize_clipped(&self.main, main_cols, pane_rows);
        self.side = resize_clipped(&self.side, side_cols.max(1), pane_rows);
        self.prompt = resize_clipped(&self.prompt, cols, PROMPT_ROWS);

        // Scratch grids also resize (clear) so the next diff is a full repaint.
        // Use SCRATCH_SENTINEL to match the initialization in `new()`, ensuring
        // that the scratch grid stays distinct from the live grid post-draw.
        self.scratch_main = Grid::new_filled(main_cols, pane_rows, SCRATCH_SENTINEL);
        self.scratch_side = Grid::new_filled(side_cols.max(1), pane_rows, SCRATCH_SENTINEL);
        self.scratch_prompt = Grid::new_filled(cols, PROMPT_ROWS, SCRATCH_SENTINEL);
    }

    /// Show or hide the side panel, reflowing the panes only on a state change.
    ///
    /// A no-op when already in the requested state, so callers can drive it from
    /// the render loop every frame cheaply; the (re)allocation only happens on
    /// the visible↔hidden transition. Reuses the current `cols`/`rows`.
    pub fn set_side_visible(&mut self, visible: bool) {
        if self.side_visible != visible {
            self.resize(self.cols, self.rows, visible);
        }
    }

    /// Mutable reference to the main pane grid.
    pub fn main_grid(&mut self) -> &mut Grid {
        &mut self.main
    }

    /// Mutable reference to the side panel grid.
    pub fn side_grid(&mut self) -> &mut Grid {
        &mut self.side
    }

    /// Whether the side panel is currently visible. Callers can skip
    /// expensive `side_grid` rendering when this returns `false`.
    #[must_use]
    pub const fn side_visible(&self) -> bool {
        self.side_visible
    }

    /// Mutable reference to the prompt bar grid.
    pub fn prompt_grid(&mut self) -> &mut Grid {
        &mut self.prompt
    }

    /// Diff each pane against its scratch, emit translated ANSI bytes, then
    /// swap scratch ↔ live so the next call diffs against the just-rendered
    /// state.
    ///
    /// Returns an empty `Vec` when nothing has changed since the last frame.
    pub fn frame(&mut self) -> Vec<u8> {
        let pane_rows = self.rows.saturating_sub(PROMPT_ROWS);
        let side_col_offset = self.cols.saturating_sub(self.side_cols);

        // Main pane: offset (0, 0).
        let runs_main = damage::diff(&self.scratch_main, &self.main);
        let bytes_main = emit_translated(&self.main, &runs_main, 0, 0);

        // Side pane: offset (0, main_cols).
        let runs_side = damage::diff(&self.scratch_side, &self.side);
        let bytes_side = emit_translated(&self.side, &runs_side, 0, side_col_offset);

        // Prompt bar: offset (rows - 3, 0).
        let runs_prompt = damage::diff(&self.scratch_prompt, &self.prompt);
        let bytes_prompt = emit_translated(&self.prompt, &runs_prompt, pane_rows, 0);

        // Swap scratch ↔ live.
        std::mem::swap(&mut self.scratch_main, &mut self.main);
        std::mem::swap(&mut self.scratch_side, &mut self.side);
        std::mem::swap(&mut self.scratch_prompt, &mut self.prompt);

        let mut out = bytes_main;
        out.extend_from_slice(&bytes_side);
        out.extend_from_slice(&bytes_prompt);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::{push_sgr_inner, Composer};
    use crate::grid::{Attr, Cell};

    #[test]
    fn emit_skips_wide_glyph_continuation_cell() {
        // A wide glyph followed by its continuation must emit ONE glyph, so the
        // terminal cursor isn't double-advanced (which would drift the row).
        let mut c = Composer::new(10, 2);
        {
            let g = c.main_grid();
            g.put(0, 0, Cell::new('\u{4e16}', 0, 0, Attr::PLAIN));
            g.put(0, 1, Cell::continuation(0));
            g.put(0, 2, Cell::new('x', 0, 0, Attr::PLAIN));
        }
        let bytes = c.frame();
        let s = String::from_utf8(bytes).expect("utf-8");
        assert!(
            s.contains("\u{4e16}x"),
            "wide glyph and next char must be adjacent (continuation emitted nothing): {s:?}"
        );
    }

    #[test]
    fn set_side_visible_toggles_only_on_change() {
        let mut c = Composer::new(80, 24);
        assert!(!c.side_visible(), "hidden by default");
        c.set_side_visible(true);
        assert!(c.side_visible());
        let cols_with_side = c.side_grid().cols();
        c.set_side_visible(true); // no-op
        assert!(c.side_visible());
        assert_eq!(c.side_grid().cols(), cols_with_side, "no reallocation on no-op");
        c.set_side_visible(false);
        assert!(!c.side_visible());
    }

    #[test]
    fn disabled_color_keeps_attrs_drops_truecolor() {
        // The translated emit path must honor `NO_COLOR` like `ansi::emit`:
        // bold survives, 24-bit fg/bg are dropped.
        let mut out: Vec<u8> = Vec::new();
        push_sgr_inner(&mut out, 0x00FF_8040, 0x0010_2030, Attr::BOLD, false);
        let s = String::from_utf8(out).expect("SGR bytes are valid UTF-8");
        assert!(s.contains("\x1b[1m"), "bold attribute should still emit");
        assert!(!s.contains("38;2"), "foreground truecolor must be skipped");
        assert!(!s.contains("48;2"), "background truecolor must be skipped");
    }

    #[test]
    fn enabled_color_emits_truecolor() {
        // The default path stays byte-identical: color enabled → fg/bg present.
        let mut out: Vec<u8> = Vec::new();
        push_sgr_inner(&mut out, 0x00FF_8040, 0x0010_2030, Attr::BOLD, true);
        let s = String::from_utf8(out).expect("SGR bytes are valid UTF-8");
        assert!(s.contains("\x1b[1m"));
        assert!(s.contains("\x1b[38;2;255;128;64m"));
        assert!(s.contains("\x1b[48;2;16;32;48m"));
    }
}
