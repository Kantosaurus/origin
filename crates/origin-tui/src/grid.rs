//! Packed 16-byte `Cell` + row-major `Grid` with `resize` / `put` / `get` /
//! `diff`-friendly raw byte access via `as_bytes`.
//!
//! `Cell` is `#[repr(C)]` so its in-memory layout matches what P4.2's SIMD
//! diff scans byte-for-byte.

use thiserror::Error;

/// Style bitflags packed into `Cell::attr`'s low byte. Higher bits are
/// reserved for future use (underline color, blink, hyperlinks, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Attr(pub u32);

impl Attr {
    pub const PLAIN: Self = Self(0);
    pub const BOLD: Self = Self(1 << 0);
    pub const ITALIC: Self = Self(1 << 1);
    pub const UNDERLINE: Self = Self(1 << 2);
    pub const REVERSE: Self = Self(1 << 3);
    pub const DIM: Self = Self(1 << 4);

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct Cell {
    /// Unicode scalar value; `Cell::BLANK` uses ASCII space.
    pub glyph: u32,
    /// Foreground color, 0x00RRGGBB; 0 means terminal default.
    pub fg: u32,
    /// Background color, 0x00RRGGBB; 0 means terminal default.
    pub bg: u32,
    /// Style flag bits (see `Attr`).
    pub attr: u32,
}

impl Cell {
    pub const BLANK: Self = Self {
        glyph: b' ' as u32,
        fg: 0,
        bg: 0,
        attr: 0,
    };

    #[must_use]
    pub const fn blank() -> Self {
        Self::BLANK
    }

    #[must_use]
    pub const fn new(ch: char, fg: u32, bg: u32, attr: Attr) -> Self {
        Self {
            glyph: ch as u32,
            fg,
            bg,
            attr: attr.0,
        }
    }

    #[must_use]
    pub const fn glyph(ch: char) -> Self {
        Self::new(ch, 0, 0, Attr::PLAIN)
    }
}

// `GridError` is the public error type and is re-exported at the crate root
// as `origin_tui::GridError`; the `Grid` prefix is intentional API naming.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum GridError {
    #[error("dimensions overflow: cols={0} rows={1}")]
    Overflow(u32, u32),
}

#[derive(Debug, Clone)]
pub struct Grid {
    cols: u16,
    rows: u16,
    cells: Vec<Cell>,
}

impl Grid {
    /// Construct a grid filled with `Cell::BLANK`.
    ///
    /// # Panics
    /// Panics if `cols * rows` overflows `usize` (unreachable on any
    /// terminal size we care about).
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        let n = usize::from(cols) * usize::from(rows);
        Self {
            cols,
            rows,
            cells: vec![Cell::BLANK; n],
        }
    }

    #[must_use]
    pub const fn cols(&self) -> u16 {
        self.cols
    }

    #[must_use]
    pub const fn rows(&self) -> u16 {
        self.rows
    }

    /// Resize and **fully clear** the buffer. All previous cell contents are
    /// lost, even within the surviving region.
    ///
    /// # Panics
    /// Panics if `cols * rows` overflows `usize` (unreachable on any
    /// terminal size we care about).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        let n = usize::from(cols) * usize::from(rows);
        self.cells.clear();
        self.cells.resize(n, Cell::BLANK);
    }

    /// Set a single cell. Out-of-bounds is a silent no-op.
    pub fn put(&mut self, row: u16, col: u16, cell: Cell) {
        if let Some(slot) = self.idx(row, col).and_then(|i| self.cells.get_mut(i)) {
            *slot = cell;
        }
    }

    /// Read a cell. Out-of-bounds returns `Cell::BLANK`.
    #[must_use]
    pub fn get(&self, row: u16, col: u16) -> Cell {
        self.idx(row, col)
            .and_then(|i| self.cells.get(i))
            .copied()
            .unwrap_or(Cell::BLANK)
    }

    /// Raw byte view — for SIMD diff (P4.2). Length is always
    /// `cols * rows * 16`.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `Cell` is `#[repr(C)]` with size 16 and no padding; a slice
        // of `Cell` aliases a slice of `u8` of `len * 16` bytes safely.
        unsafe {
            std::slice::from_raw_parts(
                self.cells.as_ptr().cast::<u8>(),
                self.cells.len() * std::mem::size_of::<Cell>(),
            )
        }
    }

    /// Number of cells. Useful for tests + benches.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    fn idx(&self, row: u16, col: u16) -> Option<usize> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        Some(usize::from(row) * usize::from(self.cols) + usize::from(col))
    }
}
