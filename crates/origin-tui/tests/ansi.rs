use origin_tui::ansi::emit;
use origin_tui::damage::Run;
use origin_tui::{Cell, Grid};

#[test]
fn empty_runs_emit_nothing() {
    let a = Grid::new(10, 4);
    let out = emit(&a, &[]);
    assert!(out.is_empty());
}

#[test]
fn single_glyph_run_emits_cup_and_glyph() {
    let mut g = Grid::new(20, 5);
    g.put(2, 3, Cell::glyph('A'));
    let runs = vec![Run {
        row: 2,
        col: 3,
        len: 1,
    }];
    let out = String::from_utf8(emit(&g, &runs)).expect("utf-8");
    // CSI row+1 ; col+1 H  then glyph
    assert!(out.contains("\x1b[3;4H"), "missing CUP, got: {out:?}");
    assert!(out.contains('A'));
}

#[test]
fn styled_run_emits_sgr_before_glyphs() {
    use origin_tui::Attr;
    let mut g = Grid::new(20, 5);
    let c = Cell::new('H', 0x00FF_0000, 0, Attr::BOLD);
    g.put(0, 0, c);
    g.put(0, 1, c);
    let runs = vec![Run {
        row: 0,
        col: 0,
        len: 2,
    }];
    let out = String::from_utf8(emit(&g, &runs)).expect("utf-8");
    // CSI 1;1H then SGR 1 (bold) + SGR 38;2;r;g;b
    assert!(out.contains("\x1b[1;1H"));
    assert!(out.contains("\x1b[1")); // bold on
    assert!(out.contains("38;2;255;0;0")); // fg true-color
    assert!(out.ends_with("HH") || out.contains("HH"));
}

#[test]
fn style_change_within_row_re_emits_sgr() {
    use origin_tui::Attr;
    let mut g = Grid::new(10, 1);
    g.put(0, 0, Cell::new('a', 0x00FF_0000, 0, Attr::PLAIN));
    g.put(0, 1, Cell::new('b', 0x0000_FF00, 0, Attr::PLAIN));
    let runs = vec![Run {
        row: 0,
        col: 0,
        len: 2,
    }];
    let out = String::from_utf8(emit(&g, &runs)).expect("utf-8");
    let n = out.matches("38;2;").count();
    assert_eq!(n, 2);
}
