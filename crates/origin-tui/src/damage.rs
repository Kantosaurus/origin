//! SIMD damage diff over packed `Cell` grids.
//!
//! Mechanism N8.1: scan two `Grid::as_bytes` views in 32-byte strides using
//! `wide::u8x32`. Any non-equal lane flips the row into "scanning" mode where
//! we fall back to byte-cell granularity to find the exact change span. The
//! diff emits `Run`s — `(row, col, len)` tuples covering contiguously-changed
//! cells on a single row.

use crate::Grid;
use wide::u8x32;

/// A contiguous damage region on one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub row: u16,
    pub col: u16,
    pub len: u16,
}

/// Compute the damage runs between `prev` (front) and `next` (back).
///
/// # Panics
/// Panics if the two grids do not share dimensions. Callers must `resize`
/// both grids in lockstep on terminal-size events.
#[must_use]
pub fn diff(prev: &Grid, next: &Grid) -> Vec<Run> {
    assert_eq!(
        (prev.cols(), prev.rows()),
        (next.cols(), next.rows()),
        "grid dims must match for diff",
    );
    let cols = prev.cols();
    let rows = prev.rows();
    let cell_bytes = 16usize; // size_of::<Cell>()
    let row_bytes = usize::from(cols) * cell_bytes;
    let prev_b = prev.as_bytes();
    let next_b = next.as_bytes();

    let mut out: Vec<Run> = Vec::new();

    for row in 0..rows {
        let off = usize::from(row) * row_bytes;
        let row_prev = &prev_b[off..off + row_bytes];
        let row_next = &next_b[off..off + row_bytes];

        // Coarse SIMD scan: 32-byte stride = 2 cells per vector.
        let mut byte_i = 0usize;
        let stride = 32usize;
        let mut row_changed = false;
        while byte_i + stride <= row_bytes {
            let va = u8x32::new(chunk32(row_prev, byte_i));
            let vb = u8x32::new(chunk32(row_next, byte_i));
            if va != vb {
                row_changed = true;
                break;
            }
            byte_i += stride;
        }
        if !row_changed && row_prev[byte_i..] != row_next[byte_i..] {
            row_changed = true;
        }
        if !row_changed {
            continue;
        }

        // Fine scan: per-cell equality on this row.
        let mut col = 0u16;
        while col < cols {
            let c_off = usize::from(col) * cell_bytes;
            let a = &row_prev[c_off..c_off + cell_bytes];
            let b = &row_next[c_off..c_off + cell_bytes];
            if a == b {
                col += 1;
                continue;
            }
            // Start of a run; extend while cells differ.
            let start = col;
            while col < cols {
                let c_off2 = usize::from(col) * cell_bytes;
                if row_prev[c_off2..c_off2 + cell_bytes] == row_next[c_off2..c_off2 + cell_bytes] {
                    break;
                }
                col += 1;
            }
            out.push(Run {
                row,
                col: start,
                len: col - start,
            });
        }
    }
    out
}

#[inline]
fn chunk32(row: &[u8], byte_i: usize) -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&row[byte_i..byte_i + 32]);
    buf
}
