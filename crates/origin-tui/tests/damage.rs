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
