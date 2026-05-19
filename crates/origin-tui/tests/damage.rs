use origin_tui::damage::{diff, Run};
use origin_tui::{Cell, Grid};

#[test]
fn one_cell_change_in_200x60_yields_one_run_len_1() {
    let a = Grid::new(200, 60);
    let b = {
        let mut g = a.clone();
        g.put(30, 100, Cell::glyph('x'));
        g
    };
    let runs = diff(&a, &b);
    assert_eq!(runs.len(), 1, "expected exactly one damage run");
    let r = &runs[0];
    assert_eq!(r.row, 30);
    assert_eq!(r.col, 100);
    assert_eq!(r.len, 1);

    let _ = &a;
}

#[test]
fn no_change_yields_empty_runs() {
    let a = Grid::new(64, 16);
    // Explicit clone keeps the test mirroring real diff use (two grids).
    #[allow(clippy::redundant_clone, reason = "test intent: two distinct grid values")]
    let b = a.clone();
    assert!(diff(&a, &b).is_empty());
}

#[test]
fn adjacent_changes_coalesce_into_single_run() {
    let a = Grid::new(80, 24);
    let mut b = a.clone();
    b.put(5, 10, Cell::glyph('a'));
    b.put(5, 11, Cell::glyph('b'));
    b.put(5, 12, Cell::glyph('c'));
    let runs = diff(&a, &b);
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0],
        Run {
            row: 5,
            col: 10,
            len: 3
        }
    );
}

#[test]
fn changes_on_different_rows_are_separate_runs() {
    let a = Grid::new(80, 24);
    let mut b = a.clone();
    b.put(1, 0, Cell::glyph('x'));
    b.put(2, 0, Cell::glyph('y'));
    let runs = diff(&a, &b);
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].row, 1);
    assert_eq!(runs[1].row, 2);
}

// Budget test only meaningful with optimizations on; debug builds run ~2-5x
// slower than the 250µs target. Ignored in debug, run in release/CI.
#[test]
#[cfg_attr(debug_assertions, ignore = "release-only: needs optimized build")]
#[allow(
    clippy::cast_possible_truncation,
    reason = "row/col always fit in u16: bound by `% u16` modulus"
)]
fn diff_200x60_1pct_under_budget() {
    use std::time::Instant;
    let cols = 200u16;
    let rows = 60u16;
    let a = Grid::new(cols, rows);
    let mut b = a.clone();
    let total = (usize::from(cols) * usize::from(rows)) / 100;
    for i in 0..total {
        let row = (i % usize::from(rows)) as u16;
        let col = ((i * 17) % usize::from(cols)) as u16;
        b.put(row, col, Cell::glyph('x'));
    }
    // Warm.
    let _ = diff(&a, &b);

    let n_iters = 1000;
    let start = Instant::now();
    for _ in 0..n_iters {
        let _ = diff(&a, &b);
    }
    let per = start.elapsed() / n_iters;
    assert!(per.as_micros() < 250, "diff slow: {per:?}");
}
