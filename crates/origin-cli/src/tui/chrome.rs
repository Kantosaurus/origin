// SPDX-License-Identifier: Apache-2.0
//! Persistent chrome: the top context strip + the bottom status zone.
//!
//! Two painters draw the always-visible frame around the transcript:
//! [`draw_top`] paints the context strip (`◆ origin v0.9.6   <cwd> · ⎇
//! <branch>` left, `◷ <elapsed> · ⛁ <ctx%>` right-aligned), and [`draw_status`]
//! paints the quiet spinner/phase zone above a full-width rule that seats the
//! composer — its right-aligned readout carries `<model> · ↑<in> ↓<out> ·
//! $<cost>` (the model moved down here from the top strip, and the token count
//! is split into outgoing `↑` / incoming `↓`). Both clip strictly to their
//! `Region` so a narrow terminal never overflows `region.width` — the cwd is
//! middle-truncated to make room for the (fixed-width, right-aligned) clock +
//! context meter.

use origin_tui::grid::{Attr, Cell, Grid};

use super::tokens::{char_cell_width, glyph, Region, Tokens};

// `format_tokens` lives in `mod.rs` and is not part of this module's owned
// surface; the chrome zone only needs the token formatter, so a small local copy
// is kept here (mirrors `mod.rs::format_tokens`) rather than reaching into a
// sibling module's private items. The per-model context window comes from the
// shared `origin_daemon::model_window::model_context_window` resolver via
// `App::ctx_pct`, which arrives pre-computed in `ChromeCtx`, so the resolver is
// not needed here.

/// Clock glyph for the right-aligned session timer (`◷`).
const CLOCK: char = '\u{25F7}';
/// Context-fill glyph for the right-aligned window meter (`⛁`).
const CTX: char = '\u{26C1}';
/// Branch glyph preceding the git branch name (`⎇`).
const BRANCH: char = '\u{2387}';
/// Inline separator between context fields (` · `).
const SEP: &str = " \u{00B7} ";
/// Crate version string (e.g. `v0.9.6`), rendered after the `◆ origin`
/// wordmark in the top strip so users can always see which build they're on.
/// Sourced from the workspace `version` via Cargo's `CARGO_PKG_VERSION`.
const VERSION_TAG: &str = concat!("v", env!("CARGO_PKG_VERSION"));
/// Context-fill thresholds (percent) where the meter turns warn / err.
const CTX_WARN_PCT: u8 = 75;
const CTX_ERR_PCT: u8 = 90;
/// Outgoing-tokens glyph for the status metrics (`↑`, input/prompt tokens).
const TOK_UP: char = '\u{2191}';
/// Incoming-tokens glyph for the status metrics (`↓`, output/completion tokens).
const TOK_DOWN: char = '\u{2193}';

/// Format a token count compactly (`1.2k`, `3.4M`). Local copy of
/// `mod.rs::format_tokens` so this module is self-contained and testable.
fn format_tokens(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", f64::from(n) / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", f64::from(n) / 1_000.0)
    } else {
        n.to_string()
    }
}

/// The data the top context strip renders: wordmark context (cwd · branch) on
/// the left, session clock + context-fill on the right. The model name moved
/// down to the bottom status zone (next to the token metrics) so the top strip
/// stays a quiet identity + location strip.
#[derive(Debug, Clone, Default)]
pub struct ChromeCtx {
    /// Current working directory, truncated middle on narrow widths.
    pub cwd: String,
    /// Git branch, if resolvable (`⎇ branch`); `None` outside a repo.
    pub branch: Option<String>,
    /// Pre-formatted session clock (e.g. `1m 04s`), from `usage.elapsed` plus
    /// any in-flight `turn_started`.
    pub elapsed: String,
    /// Context-window fill percentage (0–100), colorized warn/err past
    /// thresholds. Derived from `last_ctx_tokens` /
    /// `origin_daemon::model_window::model_context_window(model)`.
    pub ctx_pct: u8,
}

/// The data the bottom status zone renders: the quiet spinner/phase/metrics
/// line above the composer's rule.
#[derive(Debug, Clone, Default)]
pub struct StatusCtx {
    /// Current spinner frame while a turn is in flight; `None` when idle.
    pub spinner: Option<String>,
    /// A short phase label (e.g. `thinking`, goal status), if any.
    pub phase: Option<String>,
    /// Active model name (from `UsageSnapshot::model`). Rendered just before the
    /// token metrics so the build's model reads alongside its token spend.
    pub model: String,
    /// Outgoing tokens this session (input / prompt), for the `↑` metric.
    pub input_tokens: u32,
    /// Incoming tokens this session (output / completion), for the `↓` metric.
    pub output_tokens: u32,
    /// Session cost in USD, if cost tracking is on.
    pub cost: Option<f64>,
    /// Whether a turn is currently in flight (dims/animates the zone).
    pub in_flight: bool,
}

/// Display width (terminal cells) of a string.
fn str_width(s: &str) -> u16 {
    s.chars().map(char_cell_width).fold(0u16, u16::saturating_add)
}

/// A foreground/background/attr triple — the cell style for [`put_str`], kept
/// as one argument so the writer stays under clippy's arity limit.
#[derive(Debug, Clone, Copy)]
struct Cs {
    fg: u32,
    bg: u32,
    attr: Attr,
}

impl Cs {
    /// Plain-attribute style.
    const fn plain(fg: u32, bg: u32) -> Self {
        Self {
            fg,
            bg,
            attr: Attr::PLAIN,
        }
    }

    /// Style in `fg` with a given attr (transparent background).
    const fn fga(fg: u32, attr: Attr) -> Self {
        Self { fg, bg: 0, attr }
    }
}

/// Write `text` at `row`/`col` in style `cs`, advancing and returning the next
/// column. Wide glyphs emit a continuation cell. Stops at `max_col` (exclusive)
/// without splitting a wide glyph across the boundary.
fn put_str(grid: &mut Grid, row: u16, mut col: u16, max_col: u16, text: &str, cs: Cs) -> u16 {
    for ch in text.chars() {
        let w = char_cell_width(ch);
        if w == 0 {
            continue;
        }
        if col + w > max_col {
            break;
        }
        grid.put(row, col, Cell::new(ch, cs.fg, cs.bg, cs.attr));
        if w == 2 {
            grid.put(row, col + 1, Cell::continuation(cs.bg));
        }
        col += w;
    }
    col
}

/// Middle-truncate `s` to at most `budget` cells, inserting `…` at the elision
/// point so the head (drive/root) and the tail (leaf dir) both stay legible.
/// Returns `s` unchanged when it already fits, and `""` when `budget == 0`.
fn middle_truncate(s: &str, budget: u16) -> String {
    let w = str_width(s);
    if w <= budget {
        return s.to_string();
    }
    if budget == 0 {
        return String::new();
    }
    if budget == 1 {
        return "\u{2026}".to_string(); // a single ellipsis
    }
    // Reserve one cell for the ellipsis; split the rest between head and tail,
    // favoring the tail (the working leaf is the more useful half).
    let avail = budget - 1;
    let tail_budget = avail - avail / 2; // ceil(avail/2)
    let head_budget = avail / 2;

    let chars: Vec<char> = s.chars().collect();

    // Take head chars up to head_budget cells.
    let mut head = String::new();
    let mut hw = 0u16;
    for &c in &chars {
        let cw = char_cell_width(c);
        if hw + cw > head_budget {
            break;
        }
        head.push(c);
        hw += cw;
    }

    // Take tail chars (from the end) up to tail_budget cells.
    let mut tail_rev = String::new();
    let mut tw = 0u16;
    for &c in chars.iter().rev() {
        let cw = char_cell_width(c);
        if tw + cw > tail_budget {
            break;
        }
        tail_rev.push(c);
        tw += cw;
    }
    let tail: String = tail_rev.chars().rev().collect();

    format!("{head}\u{2026}{tail}")
}

/// Color for the context-fill percentage: body until ~75%, warn past 75%, err
/// past 90% — so a filling window reads as a quiet→loud signal.
const fn ctx_color(pct: u8, tok: &Tokens) -> u32 {
    if pct >= CTX_ERR_PCT {
        tok.err
    } else if pct >= CTX_WARN_PCT {
        tok.warn
    } else {
        tok.body
    }
}

/// Paint the persistent top context strip into `region`.
///
/// Left, starting at `region.left`: `◆ origin v<ver>` (accent wordmark + dim
/// version tag) then `cwd · ⎇ branch` in muted text with the cwd
/// middle-truncated to whatever width is left after reserving the right segment.
/// (The model name moved to the bottom status zone, beside the token metrics.)
/// Right, ending at `region.right()`: `◷
/// elapsed · ⛁ ctx%` with the percentage colorized warn/err past the
/// thresholds. The whole row is clipped to `region.width`, so it never
/// overflows `grid.cols()`.
pub fn draw_top(grid: &mut Grid, region: Region, ctx: &ChromeCtx, tok: &Tokens) {
    if region.width == 0 || region.height == 0 {
        return;
    }
    let row = region.top;
    let left = region.left;
    let right = region.right().min(grid.cols());
    if right <= left {
        return;
    }

    // --- Right segment: ◷ <elapsed> · ⛁ <ctx%> (fixed content, right-aligned).
    let pct_str = format!("{}%", ctx.ctx_pct);
    let right_width = {
        // "◷ " + elapsed + " · " + "⛁ " + pct
        let mut w = char_cell_width(CLOCK) + 1; // glyph + space
        w = w.saturating_add(str_width(&ctx.elapsed));
        w = w.saturating_add(str_width(SEP));
        w = w.saturating_add(char_cell_width(CTX) + 1);
        w = w.saturating_add(str_width(&pct_str));
        w
    };

    // The right segment is painted only if it fits within the region; otherwise
    // the left wordmark wins the space (a too-narrow terminal still shows ◆).
    let right_fits = right_width < region.width;
    let right_start = if right_fits {
        right.saturating_sub(right_width)
    } else {
        right
    };

    // --- Left segment: ◆ origin   <cwd> · ⎇ <branch>.
    // Wordmark first (always shown when any width exists).
    let mut col = left;
    let left_limit = if right_fits {
        // Leave a one-cell gap before the right segment.
        right_start.saturating_sub(1)
    } else {
        right
    };

    let accent_bold = Cs::fga(tok.accent, Attr::BOLD);
    let muted = Cs::plain(tok.muted, 0);
    let body = Cs::plain(tok.body, 0);

    col = put_str(
        grid,
        row,
        col,
        left_limit,
        &glyph::ORIGIN.to_string(),
        accent_bold,
    );
    col = put_str(grid, row, col, left_limit, " origin", accent_bold);

    // Version tag (` v0.9.6`) immediately after the wordmark, in dim text so it
    // reads as a quiet annotation rather than part of the identity. Only painted
    // when it fits within the left limit, so a narrow terminal keeps `◆ origin`.
    if col < left_limit {
        col = put_str(
            grid,
            row,
            col,
            left_limit,
            &format!(" {VERSION_TAG}"),
            Cs::fga(tok.muted, Attr::DIM),
        );
    }

    // Build the trailing context (cwd · ⎇ branch). The cwd is the flexible
    // field; the branch segment is fixed, so reserve it and give the remainder
    // to a middle-truncated cwd.
    if col < left_limit {
        // Two spaces between wordmark and context for breathing room.
        col = put_str(grid, row, col, left_limit, "  ", Cs::plain(0, 0));
    }

    let branch_seg_width = ctx.branch.as_ref().map_or(0u16, |b| {
        str_width(SEP) + char_cell_width(BRANCH) + 1 + str_width(b)
    });

    // Remaining cells for the whole "cwd · ⎇ branch" run.
    let remaining = left_limit.saturating_sub(col);
    // Budget for the cwd after the fixed branch segment.
    let cwd_budget = remaining.saturating_sub(branch_seg_width);
    let cwd_shown = middle_truncate(&ctx.cwd, cwd_budget);

    if !cwd_shown.is_empty() {
        col = put_str(grid, row, col, left_limit, &cwd_shown, body);
    }
    if let Some(branch) = &ctx.branch {
        col = put_str(grid, row, col, left_limit, SEP, muted);
        col = put_str(
            grid,
            row,
            col,
            left_limit,
            &format!("{BRANCH} "),
            Cs::plain(tok.accent_dim, 0),
        );
        let _ = put_str(grid, row, col, left_limit, branch, muted);
    }

    // --- Paint the right segment last (it owns the right edge regardless of
    // how far the left run reached).
    if right_fits {
        let mut rc = right_start;
        rc = put_str(grid, row, rc, right, &format!("{CLOCK} "), muted);
        rc = put_str(grid, row, rc, right, &ctx.elapsed, body);
        rc = put_str(grid, row, rc, right, SEP, muted);
        rc = put_str(grid, row, rc, right, &format!("{CTX} "), muted);
        let _ = put_str(
            grid,
            row,
            rc,
            right,
            &pct_str,
            Cs::fga(ctx_color(ctx.ctx_pct, tok), Attr::BOLD),
        );
    }
}

/// Paint the bottom status zone into `region` (above the composer rule).
///
/// The first row of `region` carries the quiet readout — spinner, phase, and a
/// right-aligned `<model> · ↑<in> ↓<out> · $<cost>` metrics group, all in
/// muted/dim text so the zone never competes with the transcript. `↑` is
/// outgoing (input/prompt) tokens, `↓` is incoming (output/completion) tokens.
/// The last row of `region` is a full-width `─` rule in `accent_dim` that seats
/// the composer beneath it. When the region is a single row, the rule wins (the
/// composer frame still needs its seat); the readout is dropped.
pub fn draw_status(grid: &mut Grid, region: Region, st: &StatusCtx, tok: &Tokens) {
    if region.width == 0 || region.height == 0 {
        return;
    }
    let left = region.left;
    let right = region.right().min(grid.cols());
    if right <= left {
        return;
    }
    let rule_row = region
        .bottom()
        .saturating_sub(1)
        .min(grid.rows().saturating_sub(1));

    // Readout row (only when there is a row above the rule).
    if region.height >= 2 {
        use std::fmt::Write as _;

        let row = region.top;

        // Build the right-aligned metrics: <model> · ↑<in> ↓<out> · $<cost>.
        // The model sits here now (moved off the top strip) so the build's model
        // reads alongside its token spend. `↑` = outgoing (input) tokens, `↓` =
        // incoming (output) tokens.
        let mut metrics = String::new();
        if !st.model.is_empty() {
            let _ = write!(metrics, "{}{SEP}", st.model);
        }
        let _ = write!(
            metrics,
            "{TOK_UP}{} {TOK_DOWN}{}",
            format_tokens(st.input_tokens),
            format_tokens(st.output_tokens),
        );
        if let Some(cost) = st.cost {
            let _ = write!(metrics, "{SEP}${cost:.4}");
        }
        let mw = str_width(&metrics);

        // The metrics are the PRIORITY readout: reserve their right-aligned slot
        // FIRST, then clip the spinner/phase to the space left of them. A long
        // phase (e.g. the `⎇ N/M agents` swarm readout) — or a narrowed status
        // zone when the swarm side panel steals width — can no longer push the
        // model/token/cost line off the row (it used to vanish entirely).
        let mstart = right.saturating_sub(mw).max(left);
        let phase_right = mstart; // hard stop so the phase never overwrites metrics

        let mut col = left;
        if let Some(spin) = &st.spinner {
            col = put_str(grid, row, col, phase_right, spin, Cs::plain(tok.accent, 0));
            col = put_str(grid, row, col, phase_right, " ", Cs::plain(0, 0));
        }
        if let Some(phase) = &st.phase {
            let fg = if st.in_flight { tok.body } else { tok.muted };
            let _ = put_str(grid, row, col, phase_right, phase, Cs::plain(fg, 0));
        }
        // Always render the metrics (right-aligned; clipped to the zone only when
        // the whole zone is narrower than the metrics themselves).
        let _ = put_str(grid, row, mstart, right, &metrics, Cs::fga(tok.muted, Attr::DIM));
    }

    // Full-width rule.
    let mut rc = left;
    while rc < right {
        grid.put(
            rule_row,
            rc,
            Cell::new('\u{2500}', tok.accent_dim, 0, Attr::PLAIN),
        );
        rc += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph_at(grid: &Grid, row: u16, col: u16) -> char {
        char::from_u32(grid.get(row, col).glyph).unwrap_or('\u{FFFD}')
    }

    /// Scan a row for the first column holding `target` (within `cols`).
    fn find_col(grid: &Grid, row: u16, target: char) -> Option<u16> {
        (0..grid.cols()).find(|&c| glyph_at(grid, row, c) == target)
    }

    fn sample_ctx() -> ChromeCtx {
        ChromeCtx {
            cwd: "/home/u/projects/origin".to_string(),
            branch: Some("feat/tui-rework".to_string()),
            elapsed: "1m 04s".to_string(),
            ctx_pct: 42,
        }
    }

    #[test]
    fn draw_top_places_wordmark_at_left_col() {
        let mut grid = Grid::new(100, 3);
        let tok = Tokens::default_tokens();
        let region = Region::new(0, 0, 100, 1);
        draw_top(&mut grid, region, &sample_ctx(), &tok);
        // ◆ wordmark sits at the region's left column.
        assert_eq!(glyph_at(&grid, 0, 0), glyph::ORIGIN, "◆ at col 0");
        // followed by " origin".
        assert_eq!(glyph_at(&grid, 0, 2), 'o', "wordmark text follows");
    }

    #[test]
    fn draw_top_shows_branch_glyph_when_some() {
        let mut grid = Grid::new(100, 3);
        let tok = Tokens::default_tokens();
        draw_top(&mut grid, Region::new(0, 0, 100, 1), &sample_ctx(), &tok);
        assert!(find_col(&grid, 0, BRANCH).is_some(), "⎇ present when branch Some");
    }

    #[test]
    fn draw_top_omits_branch_glyph_when_none() {
        let mut grid = Grid::new(100, 3);
        let tok = Tokens::default_tokens();
        let mut ctx = sample_ctx();
        ctx.branch = None;
        draw_top(&mut grid, Region::new(0, 0, 100, 1), &ctx, &tok);
        assert!(find_col(&grid, 0, BRANCH).is_none(), "no ⎇ when branch None");
    }

    #[test]
    fn status_metrics_survive_a_long_phase_in_a_narrow_zone() {
        // Regression: with the swarm side panel active the status zone is narrow
        // AND the phase carried the `⎇ N/M agents` readout — the model/token/cost
        // metrics used to vanish entirely (the `mw < region.width` guard dropped
        // them). They are the priority readout and must always render.
        let mut grid = Grid::new(40, 2);
        let tok = Tokens::default_tokens();
        let st = StatusCtx {
            spinner: None,
            phase: Some("\u{2387} 3/3 agents running".to_string()),
            model: "claude-opus-4-8".to_string(),
            input_tokens: 14_000,
            output_tokens: 75_300,
            cost: Some(72.027),
            in_flight: true,
        };
        draw_status(&mut grid, Region::new(0, 0, 40, 2), &st, &tok);
        let row0: String = (0..grid.cols()).map(|c| glyph_at(&grid, 0, c)).collect();
        assert!(
            row0.contains("claude-opus-4-8"),
            "model/metrics must stay visible in a narrow zone, got: {row0:?}"
        );
    }

    #[test]
    fn draw_top_right_aligns_clock_and_ctx_meter() {
        let cols: u16 = 100;
        let mut grid = Grid::new(cols, 3);
        let tok = Tokens::default_tokens();
        let ctx = sample_ctx();
        draw_top(&mut grid, Region::new(0, 0, cols, 1), &ctx, &tok);

        let clock_col = find_col(&grid, 0, CLOCK).expect("◷ present");
        let ctx_col = find_col(&grid, 0, CTX).expect("⛁ present");
        // Both live in the right portion of the row, clock before ctx-meter.
        assert!(clock_col > cols / 2, "clock right-aligned (col {clock_col})");
        assert!(ctx_col > clock_col, "⛁ after ◷");

        // The percentage's last digit sits at the very right edge.
        let last = glyph_at(&grid, 0, cols - 1);
        assert_eq!(last, '%', "ctx% ends flush at the right edge");
    }

    #[test]
    fn draw_top_narrow_width_never_overflows() {
        // A battery of narrow widths: nothing may be written at or past `width`,
        // and no panic occurs (the cwd is middle-truncated to fit).
        let tok = Tokens::default_tokens();
        let ctx = sample_ctx();
        for width in 1u16..=60 {
            let cols = width + 5; // grid wider than the region to catch overflow
            let mut grid = Grid::new(cols, 3);
            draw_top(&mut grid, Region::new(0, 0, width, 1), &ctx, &tok);
            for c in width..cols {
                assert_eq!(
                    glyph_at(&grid, 0, c),
                    ' ',
                    "width={width}: col {c} (>= width) must stay blank",
                );
            }
        }
    }

    #[test]
    fn draw_top_renders_version_tag_after_wordmark() {
        let mut grid = Grid::new(120, 3);
        let tok = Tokens::default_tokens();
        draw_top(&mut grid, Region::new(0, 0, 120, 1), &sample_ctx(), &tok);
        // Collect the rendered top row as a string and confirm the version tag
        // (`v<CARGO_PKG_VERSION>`) appears right after the `◆ origin` wordmark.
        let row: String = (0..grid.cols())
            .map(|c| char::from_u32(grid.get(0, c).glyph).unwrap_or(' '))
            .collect();
        let expected = format!("v{}", env!("CARGO_PKG_VERSION"));
        assert!(
            row.contains(&expected),
            "version tag `{expected}` missing from top strip:\n{row}"
        );
        // It sits between the wordmark and the cwd context field.
        let v_at = row.find(&expected).expect("version present");
        let origin_at = row.find("origin").expect("wordmark present");
        // The cwd's leaf ("origin" dir) would collide with the wordmark text, so
        // anchor on the cwd head ("/home") instead.
        let cwd_at = row.find("/home").expect("cwd present");
        assert!(origin_at < v_at, "version tag follows the wordmark");
        assert!(v_at < cwd_at, "version tag precedes the cwd");
    }

    #[test]
    fn draw_top_offset_region_keeps_wordmark_at_region_left() {
        let mut grid = Grid::new(120, 3);
        let tok = Tokens::default_tokens();
        // Region starts at col 4, row 1.
        draw_top(&mut grid, Region::new(1, 4, 90, 1), &sample_ctx(), &tok);
        assert_eq!(glyph_at(&grid, 1, 4), glyph::ORIGIN, "◆ at region.left");
        // Nothing painted to the left of the region.
        assert_eq!(glyph_at(&grid, 1, 3), ' ', "left of region untouched");
    }

    #[test]
    fn ctx_color_crosses_warn_and_err_thresholds() {
        let tok = Tokens::default_tokens();
        assert_eq!(ctx_color(50, &tok), tok.body, "<75% is body");
        assert_eq!(ctx_color(74, &tok), tok.body, "just under warn");
        assert_eq!(ctx_color(75, &tok), tok.warn, "75% is warn");
        assert_eq!(ctx_color(89, &tok), tok.warn, "just under err");
        assert_eq!(ctx_color(90, &tok), tok.err, "90% is err");
        assert_eq!(ctx_color(100, &tok), tok.err, "full is err");
    }

    #[test]
    fn draw_top_colorizes_ctx_pct_when_high() {
        let cols: u16 = 100;
        let tok = Tokens::default_tokens();
        let mut ctx = sample_ctx();
        ctx.ctx_pct = 95;
        let mut grid = Grid::new(cols, 3);
        draw_top(&mut grid, Region::new(0, 0, cols, 1), &ctx, &tok);
        // The trailing '%' carries the err color at 95%.
        assert_eq!(grid.get(0, cols - 1).fg, tok.err, "high ctx% painted err");
    }

    #[test]
    fn middle_truncate_fits_and_marks_elision() {
        // Already fits ⇒ unchanged.
        assert_eq!(middle_truncate("abc", 10), "abc");
        // Too long ⇒ contains an ellipsis and never exceeds the budget.
        let out = middle_truncate("/home/user/projects/origin/crates/x", 12);
        assert!(out.contains('\u{2026}'), "elision marker inserted");
        assert!(str_width(&out) <= 12, "truncated within budget");
        // Keeps head + tail context around the ellipsis.
        assert!(out.starts_with('/'), "head kept");
        assert!(out.ends_with('x'), "tail kept");
    }

    #[test]
    fn draw_status_paints_full_width_rule_on_last_row() {
        let cols: u16 = 40;
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(cols, 5);
        let st = StatusCtx {
            spinner: Some("\u{280B}".to_string()),
            phase: Some("thinking".to_string()),
            model: "opus-4.8".to_string(),
            input_tokens: 12_345,
            output_tokens: 6_789,
            cost: Some(0.0421),
            in_flight: true,
        };
        // 2-row status zone at rows 2..4 ⇒ rule on row 3.
        draw_status(&mut grid, Region::new(2, 0, cols, 2), &st, &tok);
        for c in 0..cols {
            assert_eq!(glyph_at(&grid, 3, c), '\u{2500}', "─ across the rule row col {c}");
            assert_eq!(grid.get(3, c).fg, tok.accent_dim, "rule in accent_dim");
        }
    }

    #[test]
    fn draw_status_renders_spinner_phase_and_metrics() {
        let cols: u16 = 60;
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(cols, 5);
        let st = StatusCtx {
            spinner: Some("\u{280B}".to_string()),
            phase: Some("thinking".to_string()),
            model: "opus-4.8".to_string(),
            input_tokens: 1_500,
            output_tokens: 800,
            cost: None,
            in_flight: true,
        };
        draw_status(&mut grid, Region::new(0, 0, cols, 2), &st, &tok);
        // Spinner at col 0.
        assert_eq!(glyph_at(&grid, 0, 0), '\u{280B}', "spinner frame at left");
        // Phase text appears somewhere on the readout row.
        assert!(find_col(&grid, 0, 't').is_some(), "phase 'thinking' present");
        // The whole readout row, as text, for substring assertions.
        let row: String = (0..grid.cols()).map(|c| glyph_at(&grid, 0, c)).collect();
        // Model name moved down here, beside the metrics.
        assert!(row.contains("opus-4.8"), "model present in status zone:\n{row}");
        // Outgoing (↑) and incoming (↓) token glyphs + counts both render.
        assert!(
            find_col(&grid, 0, TOK_UP).is_some(),
            "↑ outgoing-token glyph present"
        );
        assert!(
            find_col(&grid, 0, TOK_DOWN).is_some(),
            "↓ incoming-token glyph present"
        );
        assert!(row.contains("1.5k"), "outgoing token count present:\n{row}");
        assert!(row.contains("800"), "incoming token count present:\n{row}");
    }

    #[test]
    fn draw_status_single_row_region_paints_only_rule() {
        let cols: u16 = 30;
        let tok = Tokens::default_tokens();
        let mut grid = Grid::new(cols, 3);
        // A 1-row region ⇒ rule occupies that row, no readout.
        draw_status(&mut grid, Region::new(1, 0, cols, 1), &st_idle(), &tok);
        for c in 0..cols {
            assert_eq!(glyph_at(&grid, 1, c), '\u{2500}', "rule fills the single row");
        }
    }

    fn st_idle() -> StatusCtx {
        StatusCtx {
            spinner: None,
            phase: None,
            model: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cost: None,
            in_flight: false,
        }
    }

    #[test]
    fn format_tokens_matches_mod_copy() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1_500), "1.5k");
        assert_eq!(format_tokens(2_000_000), "2.0M");
    }
}
