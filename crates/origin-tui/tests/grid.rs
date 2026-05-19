use origin_tui::{Attr, Cell, Grid};

#[test]
fn resize_clears_and_resets_dims() {
    let mut g = Grid::new(10, 4);
    g.put(0, 0, Cell::glyph('x'));
    g.resize(5, 2);
    assert_eq!(g.cols(), 5);
    assert_eq!(g.rows(), 2);
    // Resize re-initializes cells.
    assert_eq!(g.get(0, 0), Cell::blank());
}

#[test]
fn put_and_get_round_trip() {
    let mut g = Grid::new(8, 2);
    let c = Cell::new('A', 0x00FF_FFFF, 0x0000_0000, Attr::BOLD);
    g.put(1, 3, c);
    assert_eq!(g.get(1, 3), c);
}

#[test]
fn cell_is_16_bytes_packed() {
    // Layout invariant relied on by P4.2's SIMD diff.
    assert_eq!(std::mem::size_of::<Cell>(), 16);
    assert_eq!(std::mem::align_of::<Cell>(), 4);
}

#[test]
fn out_of_bounds_put_is_noop() {
    let mut g = Grid::new(4, 2);
    g.put(99, 99, Cell::glyph('z'));
    // No panic; underlying buffer unaffected.
    assert_eq!(g.get(99, 99), Cell::blank());
}
