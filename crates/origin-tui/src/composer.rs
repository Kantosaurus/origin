//! `Composer` — a three-pane layout (main, side, prompt) backed by six
//! `Grid`s: one live and one scratch per pane, enabling SIMD damage diff via
//! `damage::diff` + `ansi::emit` on each frame.

use std::io::Write as _;

use crate::damage::{self, Run};
use crate::grid::Attr;
use crate::Grid;

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
    /// Side panel starts visible; all grids are blank.
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        let side_visible = true;
        let side_cols = compute_side_cols(cols, side_visible);
        let pane_rows = rows.saturating_sub(3);
        let main_cols = cols.saturating_sub(side_cols);

        Self {
            cols,
            rows,
            side_cols,
            main: Grid::new(main_cols, pane_rows),
            side: Grid::new(side_cols.max(1), pane_rows),
            prompt: Grid::new(cols, 3),
            scratch_main: Grid::new(main_cols, pane_rows),
            scratch_side: Grid::new(side_cols.max(1), pane_rows),
            scratch_prompt: Grid::new(cols, 3),
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

        let pane_rows = rows.saturating_sub(3);
        let main_cols = cols.saturating_sub(side_cols);

        // Preserve existing content in the live grids (clip, not rewrap).
        self.main = resize_clipped(&self.main, main_cols, pane_rows);
        self.side = resize_clipped(&self.side, side_cols.max(1), pane_rows);
        self.prompt = resize_clipped(&self.prompt, cols, 3);

        // Scratch grids also resize (clear) so the next diff is a full repaint.
        self.scratch_main = Grid::new(main_cols, pane_rows);
        self.scratch_side = Grid::new(side_cols.max(1), pane_rows);
        self.scratch_prompt = Grid::new(cols, 3);
    }

    /// Mutable reference to the main pane grid.
    pub fn main_grid(&mut self) -> &mut Grid {
        &mut self.main
    }

    /// Mutable reference to the side panel grid.
    pub fn side_grid(&mut self) -> &mut Grid {
        &mut self.side
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
        let pane_rows = self.rows.saturating_sub(3);
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
