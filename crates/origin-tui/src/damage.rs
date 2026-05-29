// SPDX-License-Identifier: Apache-2.0
//! SIMD damage diff over packed `Cell` grids (N8.1).
//!
//! Two-pass per row: 32-byte SIMD coarse scan flips the row into a fine
//! per-cell pass that emits `Run { row, col, len }` tuples for each
//! contiguous span of changed cells.

use crate::grid::Cell;
use crate::Grid;
use wide::u8x32;

const CELL_BYTES: usize = std::mem::size_of::<Cell>();
const _: () = assert!(CELL_BYTES == 16, "SIMD coarse pass assumes Cell is 16 bytes");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub row: u16,
    pub col: u16,
    pub len: u16,
}

/// Compute damage runs between `prev` and `next`.
///
/// # Panics
/// Panics if the two grids do not share `(cols, rows)`. Callers must
/// `resize` both grids in lockstep on SIGWINCH-equivalent events.
#[must_use]
pub fn diff(prev: &Grid, next: &Grid) -> Vec<Run> {
    assert_eq!(
        (prev.cols(), prev.rows()),
        (next.cols(), next.rows()),
        "grid dims must match for diff",
    );
    let cols = prev.cols();
    let rows = prev.rows();
    let row_bytes = usize::from(cols) * CELL_BYTES;
    let prev_b = prev.as_bytes();
    let next_b = next.as_bytes();

    let mut out: Vec<Run> = Vec::new();
    for row in 0..rows {
        let off = usize::from(row) * row_bytes;
        let row_prev = &prev_b[off..off + row_bytes];
        let row_next = &next_b[off..off + row_bytes];

        let stride = 32usize;
        let mut byte_i = 0usize;
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

        let mut col = u16::try_from(byte_i / CELL_BYTES).unwrap_or(0).min(cols);
        while col < cols {
            let c_off = usize::from(col) * CELL_BYTES;
            if row_prev[c_off..c_off + CELL_BYTES] == row_next[c_off..c_off + CELL_BYTES] {
                col += 1;
                continue;
            }
            let start = col;
            while col < cols {
                let c_off2 = usize::from(col) * CELL_BYTES;
                if row_prev[c_off2..c_off2 + CELL_BYTES] == row_next[c_off2..c_off2 + CELL_BYTES] {
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

fn chunk32(s: &[u8], i: usize) -> [u8; 32] {
    s[i..i + 32]
        .try_into()
        .expect("slice length is exactly 32 by construction")
}
