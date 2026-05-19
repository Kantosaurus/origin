use origin_tui::composer::Composer;
use origin_tui::Cell;

#[test]
fn first_frame_paints_initial_contents() {
    let mut c = Composer::new(40, 10);
    c.main_grid().put(0, 0, Cell::glyph('M'));
    c.side_grid().put(0, 0, Cell::glyph('S'));
    let bytes = c.frame();
    let s = String::from_utf8(bytes).expect("utf-8");
    assert!(s.contains('M'), "main cell present");
    assert!(s.contains('S'), "side cell present");
}

#[test]
fn no_change_means_empty_frame_bytes() {
    let mut c = Composer::new(20, 4);
    let _ = c.frame();
    let bytes = c.frame();
    assert!(bytes.is_empty(), "second frame with no changes emits nothing");
}

#[test]
fn toggling_side_panel_keeps_main_unchanged() {
    let mut c = Composer::new(40, 10);
    c.resize(40, 10, true);
    c.main_grid().put(2, 5, Cell::glyph('X'));
    let _ = c.frame();
    let cell_before = c.main_grid().get(2, 5);
    c.resize(40, 10, false);
    let cell_after = c.main_grid().get(2, 5);
    assert_eq!(
        cell_before, cell_after,
        "main contents must not be rewrapped on side toggle"
    );
}
