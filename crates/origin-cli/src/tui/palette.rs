// SPDX-License-Identifier: Apache-2.0
//! Slash palette (described) + `@` file/agent mention picker popup.
//!
//! Wave 1 (Task E): paints the slash-command palette with its (previously
//! discarded) descriptions surfaced, and the `@` mention popup with a per-kind
//! glyph. `SlashItem`/`MentionItem`/`MentionKind` are coordinate-free snapshots
//! of the live `SuggestionState`; `draw_slash`/`draw_mentions` write into the
//! [`Grid`] within their [`Region`], reading every color from [`Tokens`] and
//! highlighting the selected row with `tok.sel_bg`.

// `draw_slash` (described slash palette) is wired into the draw orchestration in
// INT-2; `draw_mentions` + `MentionItem`/`MentionKind` await the `@`-picker wire
// (mention candidates are not yet threaded to the popup — see the INT-2 report),
// so the module stays partially dead until then.
#![allow(dead_code)] // draw_mentions/@-picker pending; draw_slash is live

use origin_tui::grid::{Attr, Cell, Grid};

use super::tokens::{char_cell_width, glyph, Region, Tokens};

/// Saturating `usize -> u16` (visible-row counts never exceed `u16::MAX`).
fn u16c(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// One described slash-command row in the palette.
#[derive(Debug, Clone, Default)]
pub struct SlashItem {
    /// The command/skill name (rendered in `accent`).
    pub name: String,
    /// Its description (rendered in `muted` — currently computed then discarded).
    pub desc: String,
}

/// One `@`-mention candidate row.
#[derive(Debug, Clone)]
pub struct MentionItem {
    /// The display text (path or agent name).
    pub display: String,
    /// Which kind of mention this is (drives a per-kind glyph).
    pub kind: MentionKind,
}

/// The kind of an `@`-mention candidate, driving its leading glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MentionKind {
    File,
    Dir,
    Agent,
}

impl MentionKind {
    /// The leading glyph + accent for this mention kind, drawn from the one
    /// glyph family. File ⇒ `◇`, Dir ⇒ `▸`, Agent ⇒ `⊕`.
    const fn token(self, tok: &Tokens) -> (char, u32) {
        match self {
            Self::File => (glyph::READ, tok.body),
            Self::Dir => (glyph::RUN, tok.accent),
            Self::Agent => (glyph::TASK, tok.tool),
        }
    }
}

/// Write a string left-to-right starting at `col`, clipping at `clip` (exclusive
/// column bound). Honors wide glyphs (writes a continuation cell + advances 2).
/// Returns the column just past the last glyph written.
#[allow(clippy::too_many_arguments)] // grid + position + text + clip + 3 style fields are all distinct
fn write_clip(
    grid: &mut Grid,
    row: u16,
    mut col: u16,
    text: &str,
    clip: u16,
    fg: u32,
    bg: u32,
    attr: Attr,
) -> u16 {
    for ch in text.chars() {
        if col >= clip {
            break;
        }
        let w = char_cell_width(ch);
        if w == 0 {
            continue;
        }
        if col + w > clip {
            break;
        }
        grid.put(row, col, Cell::new(ch, fg, bg, attr));
        if w == 2 {
            grid.put(row, col + 1, Cell::continuation(bg));
        }
        col += w;
    }
    col
}

/// Fill `[from, clip)` on `row` with blank cells in `bg` so a selected row reads
/// as a solid band to the region edge.
fn fill_bg(grid: &mut Grid, row: u16, mut from: u16, clip: u16, bg: u32) {
    while from < clip {
        grid.put(row, from, Cell::new(' ', 0, bg, Attr::PLAIN));
        from += 1;
    }
}

/// The visible-row window over `total` items so the selected row stays on
/// screen: returns the first index to render given the row capacity.
fn scroll_offset(total: usize, sel: usize, cap: usize) -> usize {
    if cap == 0 || total <= cap {
        return 0;
    }
    if sel < cap {
        0
    } else if sel >= total - 1 {
        total - cap
    } else {
        (sel + 1).saturating_sub(cap).min(total - cap)
    }
}

/// Paint the described slash palette into `region`, highlighting row `sel`.
///
/// Each row is `<name>   <description>` with the name in `tok.accent` and the
/// **description in `tok.muted`** (the description is otherwise computed and
/// discarded — surfacing it is the point of this painter). The selected row is
/// filled with `tok.sel_bg` and its name brightened to `tok.bright`.
pub fn draw_slash(grid: &mut Grid, region: Region, items: &[SlashItem], sel: usize, tok: &Tokens) {
    if region.width < 2 || region.height == 0 || items.is_empty() {
        return;
    }
    let cap = usize::from(region.height);
    let offset = scroll_offset(items.len(), sel, cap);
    let clip = region.right();
    let left = region.left;

    // Reserve a left gutter (1) and align descriptions in a second column past
    // the widest name (clamped so descriptions still fit).
    let name_w = items
        .iter()
        .map(|it| it.name.chars().map(char_cell_width).sum::<u16>())
        .max()
        .unwrap_or(0);
    let desc_col = left
        .saturating_add(2)
        .saturating_add(name_w)
        .saturating_add(2)
        .min(clip.saturating_sub(1));

    for (i, item) in items.iter().enumerate().skip(offset).take(cap) {
        let row = region.top + u16c(i - offset);
        if row >= region.bottom() {
            break;
        }
        let selected = i == sel;
        let row_bg = if selected { tok.sel_bg } else { tok.raised };
        let name_fg = if selected { tok.bright } else { tok.accent };
        // Paint the row band first.
        fill_bg(grid, row, left, clip, row_bg);
        // Name in accent (gutter at left+1), description in muted past name_w.
        write_clip(
            grid,
            row,
            left + 2,
            &item.name,
            desc_col.saturating_sub(1).max(left + 2),
            name_fg,
            row_bg,
            if selected { Attr::BOLD } else { Attr::PLAIN },
        );
        if !item.desc.is_empty() {
            write_clip(grid, row, desc_col, &item.desc, clip, tok.muted, row_bg, Attr::PLAIN);
        }
    }
}

/// Paint the `@` mention picker popup into `region`, highlighting row `sel`.
///
/// Each row leads with a per-kind glyph (File `◇` / Dir `▸` / Agent `⊕`) then
/// the display path/name in `tok.body`. The selected row is filled with
/// `tok.sel_bg`.
pub fn draw_mentions(
    grid: &mut Grid,
    region: Region,
    items: &[MentionItem],
    sel: usize,
    tok: &Tokens,
) {
    if region.width < 2 || region.height == 0 || items.is_empty() {
        return;
    }
    let cap = usize::from(region.height);
    let offset = scroll_offset(items.len(), sel, cap);
    let clip = region.right();
    let left = region.left;

    for (i, item) in items.iter().enumerate().skip(offset).take(cap) {
        let row = region.top + u16c(i - offset);
        if row >= region.bottom() {
            break;
        }
        let selected = i == sel;
        let row_bg = if selected { tok.sel_bg } else { tok.raised };
        let (kind_glyph, glyph_fg) = item.kind.token(tok);
        let text_fg = if selected { tok.bright } else { tok.body };

        fill_bg(grid, row, left, clip, row_bg);
        // Kind glyph in its accent at the gutter (col left+1), then the display
        // text after a one-cell gap.
        let glyph_col = left + 1;
        if glyph_col < clip {
            grid.put(row, glyph_col, Cell::new(kind_glyph, glyph_fg, row_bg, Attr::PLAIN));
        }
        write_clip(
            grid,
            row,
            glyph_col + 2,
            &item.display,
            clip,
            text_fg,
            row_bg,
            if selected { Attr::BOLD } else { Attr::PLAIN },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph_at(grid: &Grid, row: u16, col: u16) -> char {
        char::from_u32(grid.get(row, col).glyph).unwrap_or('\u{FFFD}')
    }

    fn row_chars(grid: &Grid, row: u16) -> String {
        (0..grid.cols()).map(|c| glyph_at(grid, row, c)).collect()
    }

    #[test]
    fn draw_slash_surfaces_description() {
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(60, 3);
        let region = Region::new(0, 0, 60, 3);
        let items = vec![
            SlashItem {
                name: "/clear".to_string(),
                desc: "reset the conversation".to_string(),
            },
            SlashItem {
                name: "/effort".to_string(),
                desc: "set reasoning depth".to_string(),
            },
        ];
        draw_slash(&mut grid, region, &items, 0, &tok);
        // The description substring appears on the first row...
        let r0 = row_chars(&grid, 0);
        assert!(r0.contains("reset the conversation"), "desc surfaced: {r0:?}");
        // ...and the name too.
        assert!(r0.contains("/clear"), "name present: {r0:?}");
    }

    #[test]
    fn draw_slash_paints_description_in_muted() {
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(60, 2);
        let region = Region::new(0, 0, 60, 2);
        let items = vec![SlashItem {
            name: "/clear".to_string(),
            desc: "reset".to_string(),
        }];
        draw_slash(&mut grid, region, &items, 0, &tok);
        // Locate the 'r' of "reset" and confirm it's painted in muted.
        let mut desc_fg = None;
        for c in 0..grid.cols() {
            let cell = grid.get(0, c);
            // Description starts after the name; first 'r' past col 2 is "reset".
            if cell.glyph == 'r' as u32 && c > 8 {
                desc_fg = Some(cell.fg);
                break;
            }
        }
        assert_eq!(desc_fg, Some(tok.muted), "description painted in muted");
    }

    #[test]
    fn draw_slash_highlights_selected_row_with_sel_bg() {
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(40, 3);
        let region = Region::new(0, 0, 40, 3);
        let items = vec![
            SlashItem {
                name: "/a".to_string(),
                desc: "first".to_string(),
            },
            SlashItem {
                name: "/b".to_string(),
                desc: "second".to_string(),
            },
        ];
        draw_slash(&mut grid, region, &items, 1, &tok);
        // The selected (second) row's band is filled with sel_bg.
        assert_eq!(grid.get(1, 0).bg, tok.sel_bg, "selected row uses sel_bg");
        // The unselected row uses the raised surface, not sel_bg.
        assert_eq!(grid.get(0, 0).bg, tok.raised, "unselected row uses raised");
    }

    #[test]
    fn draw_mentions_shows_per_kind_glyph() {
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(50, 3);
        let region = Region::new(0, 0, 50, 3);
        let items = vec![
            MentionItem {
                display: "src/main.rs".to_string(),
                kind: MentionKind::File,
            },
            MentionItem {
                display: "src/".to_string(),
                kind: MentionKind::Dir,
            },
            MentionItem {
                display: "reviewer".to_string(),
                kind: MentionKind::Agent,
            },
        ];
        draw_mentions(&mut grid, region, &items, 0, &tok);
        // Each row leads with its kind glyph in the gutter (col 1).
        assert_eq!(glyph_at(&grid, 0, 1), glyph::READ, "File glyph ◇");
        assert_eq!(glyph_at(&grid, 1, 1), glyph::RUN, "Dir glyph ▸");
        assert_eq!(glyph_at(&grid, 2, 1), glyph::TASK, "Agent glyph ⊕");
        // The display text follows the glyph.
        assert!(row_chars(&grid, 0).contains("main.rs"), "display rendered");
    }

    #[test]
    fn draw_mentions_highlights_selected_row_with_sel_bg() {
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(50, 2);
        let region = Region::new(0, 0, 50, 2);
        let items = vec![
            MentionItem {
                display: "a.rs".to_string(),
                kind: MentionKind::File,
            },
            MentionItem {
                display: "b.rs".to_string(),
                kind: MentionKind::File,
            },
        ];
        draw_mentions(&mut grid, region, &items, 1, &tok);
        assert_eq!(grid.get(1, 0).bg, tok.sel_bg, "selected mention row uses sel_bg");
        assert_eq!(grid.get(0, 0).bg, tok.raised, "unselected mention row uses raised");
    }

    #[test]
    fn palettes_no_panic_on_empty_or_tiny() {
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(10, 4);
        draw_slash(&mut grid, Region::new(0, 0, 10, 2), &[], 0, &tok);
        draw_mentions(&mut grid, Region::new(0, 0, 10, 2), &[], 0, &tok);
        // Degenerate region.
        let items = vec![SlashItem {
            name: "/x".to_string(),
            desc: "y".to_string(),
        }];
        draw_slash(&mut grid, Region::new(0, 0, 1, 0), &items, 0, &tok);
    }
}
