use origin_stream::{TokenEvent, TokenKind};
use origin_tui::stream_widget::{Rect, StreamWidget};
use origin_tui::{Cell, Grid};

fn text_event(s: &str) -> TokenEvent {
    TokenEvent::new(TokenKind::TextDelta, s.as_bytes().to_vec())
}

#[test]
fn ascii_text_delta_lays_into_grid() {
    let mut grid = Grid::new(20, 4);
    let mut w = StreamWidget::new(Rect {
        row: 0,
        col: 0,
        cols: 20,
        rows: 4,
    });
    w.apply(&text_event("hello"), &mut grid);
    assert_eq!(grid.get(0, 0), Cell::glyph('h'));
    assert_eq!(grid.get(0, 4), Cell::glyph('o'));
    assert_eq!(grid.get(0, 5), Cell::blank());
}

#[test]
fn wraps_at_viewport_cols() {
    let mut grid = Grid::new(5, 3);
    let mut w = StreamWidget::new(Rect {
        row: 0,
        col: 0,
        cols: 5,
        rows: 3,
    });
    w.apply(&text_event("abcdefg"), &mut grid);
    assert_eq!(grid.get(0, 4), Cell::glyph('e'));
    assert_eq!(grid.get(1, 0), Cell::glyph('f'));
    assert_eq!(grid.get(1, 1), Cell::glyph('g'));
}

#[test]
fn cjk_double_width_skips_a_column() {
    let mut grid = Grid::new(6, 2);
    let mut w = StreamWidget::new(Rect {
        row: 0,
        col: 0,
        cols: 6,
        rows: 2,
    });
    w.apply(&text_event("a漢b"), &mut grid);
    assert_eq!(grid.get(0, 0), Cell::glyph('a'));
    assert_eq!(grid.get(0, 1), Cell::glyph('漢'));
    // Column 2 is the right half of the wide glyph — left blank
    // (caller decides whether to skip or fill).
    assert_eq!(grid.get(0, 3), Cell::glyph('b'));
}

#[test]
fn turn_end_advances_to_new_row() {
    let mut grid = Grid::new(8, 3);
    let mut w = StreamWidget::new(Rect {
        row: 0,
        col: 0,
        cols: 8,
        rows: 3,
    });
    w.apply(&text_event("ab"), &mut grid);
    w.apply(&TokenEvent::new(TokenKind::TurnEnd, Vec::new()), &mut grid);
    w.apply(&text_event("cd"), &mut grid);
    assert_eq!(grid.get(0, 0), Cell::glyph('a'));
    assert_eq!(grid.get(1, 0), Cell::glyph('c'));
}

#[test]
fn non_text_events_are_no_op() {
    let mut grid = Grid::new(4, 1);
    let mut w = StreamWidget::new(Rect {
        row: 0,
        col: 0,
        cols: 4,
        rows: 1,
    });
    w.apply(&TokenEvent::new(TokenKind::Usage, b"{}".to_vec()), &mut grid);
    w.apply(
        &TokenEvent::new(TokenKind::ToolUseStart, b"id\0name".to_vec()),
        &mut grid,
    );
    assert_eq!(grid.get(0, 0), Cell::blank());
}
