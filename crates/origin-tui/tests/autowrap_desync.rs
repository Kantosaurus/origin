// SPDX-License-Identifier: Apache-2.0
//! Regression: the absolute-CUP damage-diff renderer must run with terminal
//! autowrap (DECAWM) **disabled**.
//!
//! `Composer::frame()` repaints only the cells that changed since the last frame
//! and positions each run with an absolute cursor-position (CUP) escape. That is
//! only sound if the physical screen never moves under it. With autowrap ON,
//! emitting the screen's **bottom-right** cell makes an *immediate-wrap* terminal
//! (legacy Windows conhost) scroll the alternate screen up by one line. The
//! `scratch` shadow grid still believes the old layout, so the next diff never
//! repaints the scrolled-away content — producing the stale, "disjointed"
//! fragments and un-cleared highlights seen in the bug report.
//!
//! This test feeds the Composer's *real* emitted bytes through a tiny VT model
//! and shows the desync appears under autowrap and disappears without it — which
//! is exactly why `origin` disables DECAWM for the TUI session (see the
//! `origin-cli` terminal setup / `restore_terminal`).

use origin_tui::{Cell, Composer};

/// Minimal terminal model: just enough CSI handling (CUP + ignore SGR) to place
/// the glyphs the emitter writes, plus an *immediate* end-of-line wrap that
/// scrolls at the bottom row — the conhost behaviour that exposes the bug.
struct Vt {
    cols: usize,
    rows: usize,
    cur_r: usize,
    cur_c: usize,
    autowrap: bool,
    screen: Vec<Vec<char>>,
}

impl Vt {
    fn new(cols: u16, rows: u16, autowrap: bool) -> Self {
        Self {
            cols: cols as usize,
            rows: rows as usize,
            cur_r: 0,
            cur_c: 0,
            autowrap,
            screen: vec![vec![' '; cols as usize]; rows as usize],
        }
    }

    fn at(&self, r: usize, c: usize) -> char {
        self.screen[r][c]
    }

    fn scroll_up(&mut self) {
        self.screen.remove(0);
        self.screen.push(vec![' '; self.cols]);
    }

    fn put(&mut self, ch: char) {
        if self.cur_r < self.rows && self.cur_c < self.cols {
            self.screen[self.cur_r][self.cur_c] = ch;
        }
        self.cur_c += 1;
        if self.cur_c >= self.cols {
            if self.autowrap {
                self.cur_c = 0;
                self.cur_r += 1;
                if self.cur_r >= self.rows {
                    self.scroll_up();
                    self.cur_r = self.rows - 1;
                }
            } else {
                // No wrap: clamp at the last column. The emitter always re-CUPs
                // before the next run, so the clamp is never observed.
                self.cur_c = self.cols - 1;
            }
        }
    }

    /// Process a byte stream containing the only escapes the emitter produces:
    /// CSI CUP (`ESC [ row ; col H`) and SGR (`ESC [ ... m`, no screen effect).
    fn feed(&mut self, bytes: &[u8]) {
        let s = std::str::from_utf8(bytes).expect("emitter output is valid UTF-8");
        let mut it = s.chars().peekable();
        while let Some(ch) = it.next() {
            if ch == '\u{1b}' {
                if it.peek() == Some(&'[') {
                    it.next();
                    let mut params = String::new();
                    let mut final_byte = ' ';
                    for c2 in it.by_ref() {
                        if c2.is_ascii_alphabetic() {
                            final_byte = c2;
                            break;
                        }
                        params.push(c2);
                    }
                    if final_byte == 'H' || final_byte == 'f' {
                        let mut parts = params.split(';');
                        let row: usize = parts.next().and_then(|p| p.parse().ok()).unwrap_or(1);
                        let col: usize = parts.next().and_then(|p| p.parse().ok()).unwrap_or(1);
                        self.cur_r = row.saturating_sub(1).min(self.rows.saturating_sub(1));
                        self.cur_c = col.saturating_sub(1).min(self.cols.saturating_sub(1));
                    }
                    // Any other CSI (SGR `m`, etc.) has no effect on cell content.
                }
                continue;
            }
            self.put(ch);
        }
    }
}

/// Render two frames through a real `Composer` and return the resulting VT
/// screen.
///
/// * Frame A draws `XY` on the top row and fills the prompt row, so the screen's
///   bottom-right cell *is* written.
/// * Frame B changes only the `Y`→`Z`, leaving `X` at (0,0) untouched — so the
///   damage diff will **not** re-emit (0,0). If frame A scrolled the screen, the
///   stale `X` is gone for good and is never restored.
fn render_two_frames(autowrap: bool) -> Vt {
    let (cols, rows) = (4u16, 3u16);
    let mut comp = Composer::new(cols, rows);

    // ── Frame A ──────────────────────────────────────────────────────────────
    {
        let main = comp.main_grid();
        main.fill(Cell::blank());
        main.put(0, 0, Cell::glyph('X'));
        main.put(0, 1, Cell::glyph('Y'));
    }
    {
        let prompt = comp.prompt_grid(); // single row at the screen bottom
        for c in 0..cols {
            prompt.put(0, c, Cell::glyph(' ')); // includes the bottom-right cell
        }
    }
    let a = comp.frame();

    // ── Frame B: only (0,1) changes ─────────────────────────────────────────
    {
        let main = comp.main_grid();
        main.fill(Cell::blank());
        main.put(0, 0, Cell::glyph('X'));
        main.put(0, 1, Cell::glyph('Z'));
    }
    {
        let prompt = comp.prompt_grid();
        for c in 0..cols {
            prompt.put(0, c, Cell::glyph(' '));
        }
    }
    let b = comp.frame();

    let mut vt = Vt::new(cols, rows, autowrap);
    vt.feed(&a);
    vt.feed(&b);
    vt
}

#[test]
fn autowrap_on_scrolls_and_desyncs_the_shadow_grid() {
    // The bug: with autowrap ON, writing the bottom-right cell in frame A scrolls
    // the screen, so the unchanged 'X' at (0,0) is lost and the diff never
    // restores it.
    let vt = render_two_frames(true);
    assert_ne!(
        vt.at(0, 0),
        'X',
        "autowrap ON should have scrolled 'X' off-screen, proving the scratch/screen desync",
    );
}

#[test]
fn autowrap_off_keeps_screen_in_sync_with_the_grid() {
    // The fix premise: with autowrap OFF, the bottom-right write does not scroll,
    // so the screen matches the logical grid frame-to-frame.
    let vt = render_two_frames(false);
    assert_eq!(
        vt.at(0, 0),
        'X',
        "autowrap OFF must keep 'X' at (0,0) — the renderer's required invariant",
    );
    assert_eq!(vt.at(0, 1), 'Z', "frame B's change must also be present");
}
