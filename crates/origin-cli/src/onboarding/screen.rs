// SPDX-License-Identifier: Apache-2.0
//! The crossterm raw-mode shell for the onboarding picker.
//!
//! This is the only onboarding module that touches the terminal. It enters raw
//! mode behind a RAII [`RawGuard`] (so the terminal is restored on **every**
//! return path — normal exit, an early `?`, or an unwinding panic), sizes a
//! [`Grid`] to the terminal each frame, paints the banner / breadcrumb / search
//! / filtered rows / hint with [`crate::tui::tokens`], emits the grid as one
//! batch of ANSI, then maps a crossterm `KeyEvent` to a [`PickKey`] and folds it
//! through [`picker::reduce`]. The render itself is split into [`paint_picker`]
//! (pure: state → `Grid`) so it can be unit-tested without a terminal.
//!
//! No new dependencies: the grid is emitted with hand-rolled VT escapes (the
//! same `\x1b[…` vocabulary `main.rs` uses), not a higher-level draw crate.

use std::fmt::Write as _;
use std::io::{self, Write};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use origin_tui::{Attr, Cell, Grid};

use crate::tui::tokens::{glyph, Tokens};

use super::picker::{filtered, reduce, PickKey, PickResult, PickerState};

/// RAII guard: enabling raw mode on construction, disabling it on drop. Drop
/// runs on a normal return, an early `?`, and an unwinding panic alike, so the
/// terminal can never be stranded in raw mode by the picker loop.
struct RawGuard;

impl RawGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        // Best-effort, error-swallowing — Drop must not panic and there is
        // nothing useful to do if the restore itself fails.
        let _ = disable_raw_mode();
        let mut out = io::stdout();
        // Clear the painted picker frame (`\x1b[2J\x1b[H` = erase + home) so the
        // raw-mode rows don't linger under the subsequent cooked-mode prompts,
        // then reset SGR, show the cursor, and move to a fresh line so the next
        // prompt/log lands cleanly on a blank screen.
        let _ = out.write_all(b"\x1b[2J\x1b[H\x1b[0m\x1b[?25h\r\n");
        let _ = out.flush();
    }
}

/// Run the interactive picker to completion.
///
/// Renders `banner`/`breadcrumb`/`title` plus the search bar and `state`'s
/// filtered rows, blocking on key events until a row is selected (`⏎`) or the
/// user backs out (`esc`). Raw mode is entered for the duration and restored on
/// return (see [`RawGuard`]).
///
/// # Errors
/// Propagates terminal raw-mode, size, write/flush, and event-read failures.
pub fn run_picker(
    banner: &str,
    breadcrumb: &str,
    title: &str,
    mut state: PickerState,
    tok: &Tokens,
) -> io::Result<PickResult> {
    let _guard = RawGuard::enter()?;
    let mut out = io::stdout();
    loop {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let grid = paint_picker(banner, breadcrumb, title, &state, tok, cols.max(20), rows.max(8));
        emit_grid(&mut out, &grid)?;

        let Some(key) = read_pick_key()? else {
            continue;
        };
        if let Some(result) = reduce(&mut state, key) {
            return Ok(result);
        }
    }
}

/// The result of a [`run_text_field`] interaction.
///
/// The picker flow needs to tell a *cancel* (the user pressed `esc` to step
/// back a stage) apart from a *submit* (the user pressed `⏎`, possibly with an
/// empty field). Collapsing both to `None` — as an earlier version did — made
/// `esc` on the API-key field abort onboarding instead of stepping back, so the
/// two outcomes are now explicit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldOutcome {
    /// `⏎` was pressed. Carries the trimmed field text, which may be empty.
    Submitted(String),
    /// `esc` was pressed — the caller should step back one stage, not error.
    Cancelled,
}

/// One-line masked/plain raw-mode text field for the API-key paste step.
///
/// Echoes `•` for every typed char when `masked` (so a pasted key is not shown).
/// `⏎` returns [`FieldOutcome::Submitted`] with the trimmed text (possibly the
/// empty string — the caller decides whether an empty submit is allowed); `esc`
/// returns [`FieldOutcome::Cancelled`] so the caller can step back rather than
/// abort. Raw mode is entered and restored via [`RawGuard`].
///
/// # Errors
/// Propagates terminal raw-mode and write/flush/event failures.
pub fn run_text_field(prompt: &str, masked: bool) -> io::Result<FieldOutcome> {
    let _guard = RawGuard::enter()?;
    let mut out = io::stdout();
    let mut buf = String::new();
    write!(out, "\r\n  {prompt} ")?;
    out.flush()?;
    loop {
        let Event::Key(KeyEvent { code, kind, .. }) = event::read()? else {
            continue;
        };
        if kind == KeyEventKind::Release {
            continue;
        }
        match code {
            KeyCode::Enter => {
                return Ok(FieldOutcome::Submitted(buf.trim().to_string()));
            }
            KeyCode::Esc => return Ok(FieldOutcome::Cancelled),
            KeyCode::Backspace => {
                if buf.pop().is_some() {
                    // Erase one echoed cell.
                    write!(out, "\x08 \x08")?;
                    out.flush()?;
                }
            }
            KeyCode::Char(c) => {
                buf.push(c);
                write!(out, "{}", if masked { '\u{2022}' } else { c })?;
                out.flush()?;
            }
            _ => {}
        }
    }
}

/// Read one key event and normalize it into a [`PickKey`]. Returns `Ok(None)`
/// for events the picker ignores (key releases, resizes, mouse, unmapped keys),
/// so the caller simply repaints and reads again.
fn read_pick_key() -> io::Result<Option<PickKey>> {
    let Event::Key(KeyEvent {
        code,
        modifiers,
        kind,
        ..
    }) = event::read()?
    else {
        return Ok(None);
    };
    if kind == KeyEventKind::Release {
        return Ok(None);
    }
    Ok(map_key(code, modifiers))
}

/// Pure key map: crossterm `KeyCode` (+ modifiers) → [`PickKey`].
///
/// `Ctrl-C` surfaces as `Esc` (back out); `Ctrl-N`/`Ctrl-P` mirror `↓`/`↑`.
/// Plain printable chars become `Char(..)` so they flow into the search query
/// (no `j`/`k` cycle binding, which would shadow typing those letters).
const fn map_key(code: KeyCode, modifiers: KeyModifiers) -> Option<PickKey> {
    if modifiers.contains(KeyModifiers::CONTROL) {
        return match code {
            KeyCode::Char('c') => Some(PickKey::Esc),
            KeyCode::Char('n') => Some(PickKey::Down),
            KeyCode::Char('p') => Some(PickKey::Up),
            _ => None,
        };
    }
    match code {
        KeyCode::Up => Some(PickKey::Up),
        KeyCode::Down => Some(PickKey::Down),
        KeyCode::Enter => Some(PickKey::Enter),
        KeyCode::Esc => Some(PickKey::Esc),
        KeyCode::Backspace => Some(PickKey::Backspace),
        KeyCode::Char(c) => Some(PickKey::Char(c)),
        _ => None,
    }
}

/// Paint one frame of the picker into a fresh [`Grid`]. Pure (no terminal), so
/// the layout is unit-testable: the test asserts the wordmark, search glyph, and
/// cursor marker land where expected.
///
/// Layout (matches the spec mock):
/// ```text
///  ◆ origin · breadcrumb
///  ─────────────────────────────
///  title
///
///  ⌕ query▌
///
///  ▸ label        note
///    label        note
///  ↑↓ cycle · ⏎ select · type to search · esc back
/// ```
#[must_use]
pub fn paint_picker(
    banner: &str,
    breadcrumb: &str,
    title: &str,
    state: &PickerState,
    tok: &Tokens,
    cols: u16,
    rows: u16,
) -> Grid {
    let mut grid = Grid::new(cols, rows);
    let max = cols;

    // Row 0: ◆ origin · breadcrumb
    let mut col = put_str(&mut grid, 0, 1, &format!("{} ", glyph::ORIGIN), tok.accent, max);
    col = put_str(&mut grid, 0, col, banner, tok.bright, max);
    if !breadcrumb.is_empty() {
        let _ = put_str(
            &mut grid,
            0,
            col,
            &format!(" \u{00b7} {breadcrumb}"),
            tok.muted,
            max,
        );
    }

    // Row 1: a horizontal rule.
    let rule = "\u{2500}".repeat(usize::from(cols.saturating_sub(2)));
    put_str(&mut grid, 1, 1, &rule, tok.muted, max);

    // Row 2: title.
    put_str(&mut grid, 2, 1, title, tok.body, max);

    // Row 4: search bar — `⌕ query▌`.
    let search = format!("{} {}{}", glyph::GREP, state.query, glyph::CARET);
    put_str(&mut grid, 4, 1, &search, tok.accent_dim, max);

    // Rows 6..: the filtered rows, the cursor row highlighted with sel_bg.
    let view = filtered(state);
    let list_top: u16 = 6;
    let hint_row = rows.saturating_sub(1);
    let available = hint_row.saturating_sub(list_top) as usize;
    // Scroll the window so the cursor stays visible when the list is long.
    let start = state.cursor.saturating_sub(available.saturating_sub(1));
    for (slot, &item_idx) in view.iter().enumerate().skip(start).take(available) {
        let r = list_top + u16::try_from(slot - start).unwrap_or(0);
        let row = &state.items[item_idx];
        let is_cursor = slot == state.cursor;
        let bg = if is_cursor { tok.sel_bg } else { 0 };
        let label_fg = if is_cursor { tok.bright } else { tok.body };
        // Fill the whole row's background first so the selection band is solid.
        if is_cursor {
            for c in 1..max {
                grid.put(r, c, Cell::new(' ', 0, bg, Attr::PLAIN));
            }
        }
        let marker = if is_cursor {
            format!("{} ", glyph::CURSOR)
        } else {
            "  ".to_string()
        };
        let mut c = put_styled(&mut grid, r, 1, &marker, tok.accent, bg, max);
        c = put_styled(&mut grid, r, c, &row.label, label_fg, bg, max);
        if let Some(note) = &row.note {
            // Right-pad to a small gutter, then the muted note.
            let note_col = c.saturating_add(2).max(28);
            put_styled(&mut grid, r, note_col, note, tok.muted, bg, max);
        }
    }

    // Last row: the hint line.
    let hint = "\u{2191}\u{2193} cycle \u{00b7} \u{23ce} select \u{00b7} type to search \u{00b7} esc back";
    put_str(&mut grid, hint_row, 1, hint, tok.muted, max);

    grid
}

/// Write `s` into `grid` at `(row, col)` in `fg` on the default background,
/// clipping at `max` columns and honoring wide glyphs. Returns the next free
/// column.
fn put_str(grid: &mut Grid, row: u16, col: u16, s: &str, fg: u32, max: u16) -> u16 {
    put_styled(grid, row, col, s, fg, 0, max)
}

/// Like [`put_str`] but with an explicit background (for the selection band).
fn put_styled(grid: &mut Grid, row: u16, mut col: u16, s: &str, fg: u32, bg: u32, max: u16) -> u16 {
    use crate::tui::tokens::char_cell_width;
    for ch in s.chars() {
        if col >= max {
            break;
        }
        let w = char_cell_width(ch);
        if w == 0 {
            continue;
        }
        if col + w > max {
            break;
        }
        grid.put(row, col, Cell::new(ch, fg, bg, Attr::PLAIN));
        if w == 2 {
            grid.put(row, col + 1, Cell::continuation(bg));
        }
        col += w;
    }
    col
}

/// Emit the whole grid to `out` as one batched ANSI write: clear screen, then
/// for each row move-to + per-run color + glyph. Truecolor SGR (`38;2;r;g;b`)
/// matches the TUI's color model; `0` colors mean "terminal default" and are
/// emitted as a reset so the theme's transparent cells read correctly.
fn emit_grid(out: &mut impl Write, grid: &Grid) -> io::Result<()> {
    let mut buf = String::with_capacity(usize::from(grid.cols()) * usize::from(grid.rows()) * 4);
    // Hide cursor + home + clear.
    buf.push_str("\x1b[?25l\x1b[H\x1b[2J");
    for row in 0..grid.rows() {
        let _ = write!(buf, "\x1b[{};1H", row + 1);
        // The last (fg, bg) pair emitted, so repeated colors don't re-emit SGR.
        let mut emitted: Option<(u32, u32)> = None;
        for c in 0..grid.cols() {
            let cell = grid.get(row, c);
            if cell.is_continuation() {
                continue;
            }
            if emitted != Some((cell.fg, cell.bg)) {
                push_color(&mut buf, cell.fg, true);
                push_color(&mut buf, cell.bg, false);
                emitted = Some((cell.fg, cell.bg));
            }
            let ch = char::from_u32(cell.glyph).unwrap_or(' ');
            buf.push(ch);
        }
        buf.push_str("\x1b[0m");
    }
    out.write_all(buf.as_bytes())?;
    out.flush()
}

/// Append an SGR color escape for `color` (a packed `0x00RRGGBB`). `0` ⇒ a
/// reset to the terminal default (`39` fg / `49` bg) so transparent cells honor
/// the user's terminal/`NO_COLOR` choice.
fn push_color(buf: &mut String, color: u32, fg: bool) {
    if color == 0 {
        buf.push_str(if fg { "\x1b[39m" } else { "\x1b[49m" });
        return;
    }
    let red = (color >> 16) & 0xFF;
    let green = (color >> 8) & 0xFF;
    let blue = color & 0xFF;
    let lead = if fg { 38 } else { 48 };
    let _ = write!(buf, "\x1b[{lead};2;{red};{green};{blue}m");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboarding::picker::Row;

    fn grid_char(grid: &Grid, row: u16, col: u16) -> char {
        char::from_u32(grid.get(row, col).glyph).unwrap_or('?')
    }

    fn row_string(grid: &Grid, row: u16) -> String {
        (0..grid.cols()).map(|c| grid_char(grid, row, c)).collect()
    }

    #[test]
    fn paints_wordmark_search_and_cursor_marker() {
        let tok = Tokens::default_tokens();
        let state = PickerState::new(vec![
            Row::with_note("anthropic", "Anthropic", "OAuth / API key"),
            Row::new("openai", "OpenAI"),
        ]);
        let grid = paint_picker("origin", "primary", "Choose your provider", &state, &tok, 60, 16);

        // ◆ wordmark glyph sits near the top-left (col 1).
        assert_eq!(grid_char(&grid, 0, 1), glyph::ORIGIN, "◆ wordmark at top");
        // The ⌕ search glyph is on the search row.
        assert!(
            row_string(&grid, 4).contains(glyph::GREP),
            "⌕ search glyph on row 4: {:?}",
            row_string(&grid, 4)
        );
        // The ▸ cursor marker is on the first (selected) filtered row.
        assert_eq!(grid_char(&grid, 6, 1), glyph::CURSOR, "▸ cursor on selected row");
        // The selected row shows its label.
        assert!(
            row_string(&grid, 6).contains("Anthropic"),
            "selected row label: {:?}",
            row_string(&grid, 6)
        );
    }

    #[test]
    fn cursor_marker_follows_selection() {
        let tok = Tokens::default_tokens();
        let mut state = PickerState::new(vec![Row::new("a", "Alpha"), Row::new("b", "Beta")]);
        state.cursor = 1;
        let grid = paint_picker("origin", "", "Pick", &state, &tok, 50, 14);
        // Row 6 = first item (no marker), row 7 = second item (cursor).
        assert_eq!(grid_char(&grid, 6, 1), ' ', "non-cursor row has no marker");
        assert_eq!(
            grid_char(&grid, 7, 1),
            glyph::CURSOR,
            "cursor moved to second row"
        );
    }

    #[test]
    fn filtered_query_hides_nonmatching_rows() {
        let tok = Tokens::default_tokens();
        let mut state = PickerState::new(vec![Row::new("a", "Alpha"), Row::new("b", "Beta")]);
        state.query = "alph".to_string();
        let grid = paint_picker("origin", "", "Pick", &state, &tok, 50, 14);
        assert!(row_string(&grid, 6).contains("Alpha"));
        // Only one row matches, so row 7 holds no item label.
        assert!(!row_string(&grid, 7).contains("Beta"));
    }

    #[test]
    fn scrolled_paint_keeps_cursor_on_bottom_visible_row() {
        // A long list on a short terminal: the scroll window must follow the
        // cursor so the highlighted row stays on-screen (the bottom-most visible
        // list row), locking in the `start`/`available` window math.
        let tok = Tokens::default_tokens();
        let items: Vec<Row> = (0..50)
            .map(|i| Row::new(format!("v{i}"), format!("Item {i}")))
            .collect();
        let mut state = PickerState::new(items);
        state.cursor = 47; // near the end of the 50-item list

        let rows: u16 = 12; // short terminal
        let grid = paint_picker("origin", "primary", "Pick", &state, &tok, 60, rows);

        // Layout: list starts at row 6, the hint occupies the last row
        // (rows - 1), so the bottom-most *visible* list row is hint_row - 1.
        let list_top: u16 = 6;
        let hint_row = rows - 1;
        let bottom_list_row = hint_row - 1;
        let available = (hint_row - list_top) as usize; // visible list slots

        // The cursor (item 47) renders on the bottom-most visible list row,
        // marked with the cursor glyph.
        assert_eq!(
            grid_char(&grid, bottom_list_row, 1),
            glyph::CURSOR,
            "cursor glyph on the bottom visible list row {bottom_list_row}: {:?}",
            row_string(&grid, bottom_list_row)
        );
        // And it shows the correct label there (the scroll window math placed
        // exactly `Item 47` at the bottom).
        assert!(
            row_string(&grid, bottom_list_row).contains("Item 47"),
            "bottom visible row label: {:?}",
            row_string(&grid, bottom_list_row)
        );

        // The top visible list row holds the first item of the scroll window:
        // cursor - (available - 1) = 47 - (available - 1).
        let top_item = 47 - (available - 1);
        assert!(
            row_string(&grid, list_top).contains(&format!("Item {top_item}")),
            "top visible row holds the window's first item Item {top_item}: {:?}",
            row_string(&grid, list_top)
        );
        // Earlier items scrolled off the top: Item 0 is not painted anywhere.
        for r in list_top..=bottom_list_row {
            assert!(
                !row_string(&grid, r).contains("Item 0 "),
                "Item 0 scrolled off and is not visible on row {r}"
            );
        }
    }

    #[test]
    fn field_outcome_distinguishes_cancel_from_empty_submit() {
        // The original bug collapsed `esc` and empty-`⏎` to the same `None`,
        // so `esc` on the API-key field aborted onboarding. The enum keeps them
        // distinct: an empty submit is NOT a cancel.
        let empty_submit = FieldOutcome::Submitted(String::new());
        let cancel = FieldOutcome::Cancelled;
        assert_ne!(empty_submit, cancel, "empty submit must not equal cancel");
        assert!(matches!(empty_submit, FieldOutcome::Submitted(s) if s.is_empty()));
        assert!(matches!(cancel, FieldOutcome::Cancelled));
        // A non-empty submit carries the trimmed text.
        assert_eq!(
            FieldOutcome::Submitted("sk-test".to_string()),
            FieldOutcome::Submitted("sk-test".to_string())
        );
    }

    #[test]
    fn maps_keys_to_pickkeys() {
        assert_eq!(map_key(KeyCode::Up, KeyModifiers::NONE), Some(PickKey::Up));
        assert_eq!(map_key(KeyCode::Down, KeyModifiers::NONE), Some(PickKey::Down));
        assert_eq!(map_key(KeyCode::Enter, KeyModifiers::NONE), Some(PickKey::Enter));
        assert_eq!(map_key(KeyCode::Esc, KeyModifiers::NONE), Some(PickKey::Esc));
        assert_eq!(
            map_key(KeyCode::Backspace, KeyModifiers::NONE),
            Some(PickKey::Backspace)
        );
        assert_eq!(
            map_key(KeyCode::Char('x'), KeyModifiers::NONE),
            Some(PickKey::Char('x'))
        );
        assert_eq!(
            map_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(PickKey::Esc)
        );
    }
}
