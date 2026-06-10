// SPDX-License-Identifier: Apache-2.0
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
}

#[test]
fn no_change_yields_empty_runs() {
    let a = Grid::new(64, 16);
    #[allow(clippy::redundant_clone, reason = "two distinct grid values for diff input")]
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

#[test]
#[should_panic(expected = "grid dims must match")]
fn mismatched_dims_panic() {
    let a = Grid::new(10, 5);
    let b = Grid::new(11, 5);
    let _ = diff(&a, &b);
}

#[test]
fn run_starting_on_continuation_extends_left_to_the_wide_glyph() {
    // A wide glyph occupies two cells: the glyph itself plus a continuation
    // (trailing-half) cell. If only the continuation cell differs between frames,
    // a naive diff would emit a run that STARTS on the continuation — which the
    // emitter skips without advancing the cursor, shifting the row. The run must
    // be extended left to include the wide glyph so the pair repaints together.
    let mut a = Grid::new(8, 1);
    a.put(0, 0, Cell::new('\u{4e16}', 0, 0, origin_tui::Attr::PLAIN)); // wide glyph
    a.put(0, 1, Cell::continuation(0)); // its trailing half
    let mut b = a.clone();
    // Change ONLY the continuation cell (different bg), leaving the wide glyph
    // byte-identical, so the changed region begins on the continuation cell.
    b.put(0, 1, Cell::continuation(0x0010_2030));

    let runs = diff(&a, &b);
    assert_eq!(runs.len(), 1, "exactly one changed region");
    assert_eq!(
        runs[0],
        Run {
            row: 0,
            col: 0, // extended left from col 1 to include the wide glyph
            len: 2,
        },
        "run must cover the wide glyph + its continuation",
    );
}
