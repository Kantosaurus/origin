// SPDX-License-Identifier: Apache-2.0
use origin_tui::ansi::emit;
use origin_tui::damage::Run;
use origin_tui::{Attr, Cell, Grid};

#[test]
fn empty_runs_emit_nothing() {
    let g = Grid::new(10, 4);
    assert!(emit(&g, &[]).is_empty());
}

#[test]
fn single_glyph_run_emits_cup_plus_glyph_plus_reset() {
    let mut g = Grid::new(10, 4);
    g.put(2, 5, Cell::glyph('X'));
    let bytes = emit(
        &g,
        &[Run {
            row: 2,
            col: 5,
            len: 1,
        }],
    );
    // CUP is 1-based: row=3, col=6
    let s = std::str::from_utf8(&bytes).expect("valid utf-8");
    assert!(s.starts_with("\x1b[3;6H"), "cursor position prefix; got {s:?}");
    assert!(s.contains('X'), "glyph must appear; got {s:?}");
    assert!(s.ends_with("\x1b[0m"), "SGR reset trailing; got {s:?}");
}

#[test]
fn truecolor_fg_emits_38_2_triplet() {
    let mut g = Grid::new(4, 1);
    g.put(0, 0, Cell::new('A', 0x00FF_0000, 0, Attr::PLAIN));
    let s = String::from_utf8(emit(
        &g,
        &[Run {
            row: 0,
            col: 0,
            len: 1,
        }],
    ))
    .expect("utf-8 ansi");
    assert!(s.contains("\x1b[38;2;255;0;0m"), "truecolor fg; got {s:?}");
}

#[test]
fn bold_attr_emits_sgr_1() {
    let mut g = Grid::new(4, 1);
    g.put(0, 0, Cell::new('B', 0, 0, Attr::BOLD));
    let s = String::from_utf8(emit(
        &g,
        &[Run {
            row: 0,
            col: 0,
            len: 1,
        }],
    ))
    .expect("utf-8 ansi");
    assert!(s.contains("\x1b[1m"), "bold SGR; got {s:?}");
}

#[test]
fn multi_run_resets_between_runs() {
    let mut g = Grid::new(10, 2);
    g.put(0, 0, Cell::glyph('a'));
    g.put(1, 0, Cell::glyph('b'));
    let bytes = emit(
        &g,
        &[
            Run {
                row: 0,
                col: 0,
                len: 1,
            },
            Run {
                row: 1,
                col: 0,
                len: 1,
            },
        ],
    );
    let s = std::str::from_utf8(&bytes).expect("utf-8");
    let reset_count = s.matches("\x1b[0m").count();
    assert!(
        reset_count >= 2,
        "one SGR reset per run; saw {reset_count} in {s:?}"
    );
}
