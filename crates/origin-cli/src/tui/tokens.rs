// SPDX-License-Identifier: Apache-2.0
//! Single source of truth for the TUI's colors, glyphs, and the
//! coordinate-free row primitives the painter modules emit.
//!
//! [`Tokens`] derives every named color role from a [`theme::Palette`] snapshot
//! so `/theme`, `NO_COLOR`, and the `HighContrast` variant all flow through one
//! place (Task F2). [`Region`] is a renderer-agnostic rectangle painters clip
//! to; [`RenderRow`]/[`RowSpan`] are owned styled rows a painter produces and
//! [`blit_row`] places into a [`Grid`] (Task F1). Decoupling painters from
//! absolute grid coordinates is what lets them be unit-tested and built in
//! parallel in Wave 1.

use origin_tui::grid::{Attr, Cell, Grid};

use crate::theme;

// ---------------------------------------------------------------------------
// Tokens — the single color system, derived from a theme::Palette  (Task F2)
// ---------------------------------------------------------------------------

/// The complete named-color system every painter reads from.
///
/// Derived from a [`theme::Palette`] snapshot via [`Tokens::from_palette`] so
/// `/theme`, the `HighContrast` variant, and `NO_COLOR` all flow through one
/// place. Values for the default ("Burnished Copper") theme come from the spec
/// (`docs/superpowers/specs/2026-06-16-tui-rework-design.md`), with `muted` and
/// `accent_dim` retuned upward so every text token clears a 4.5:1 contrast
/// ratio on both `bg` and `raised` (see [`contrast_ratio`] and the test below).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tokens {
    // Surfaces.
    /// Near-black warm base background.
    pub bg: u32,
    /// Raised surface (cards, status zone).
    pub raised: u32,
    /// Faint tint for code blocks / selected rows, distinct from `raised`.
    pub band: u32,
    // Accent.
    /// Copper accent — the signature spine + role color.
    pub accent: u32,
    /// Dimmed copper, retuned to stay >=4.5:1 on `raised`.
    pub accent_dim: u32,
    // Text ramp.
    /// Lowest legible text (hints, descriptions); retuned to >=4.5:1.
    pub muted: u32,
    /// Body prose.
    pub body: u32,
    /// Brightest text (emphasis, diff text).
    pub bright: u32,
    // Headings (gold ramp).
    pub h1: u32,
    pub h2: u32,
    pub h3: u32,
    // Roles.
    /// The user's turn — warm cream/amber (retoned off cornflower-blue).
    pub you: u32,
    /// The assistant's turn — copper (== `accent`).
    pub origin: u32,
    // Tools.
    /// Base tool color (purple), used when a tool has no specific accent.
    pub tool: u32,
    // Semantics.
    pub ok: u32,
    pub warn: u32,
    pub err: u32,
    // Code.
    pub code_fg: u32,
    pub code_bg: u32,
    // Structure.
    /// Copper transcript spine gutter.
    pub spine: u32,
    /// Selected-row background (palettes/pickers).
    pub sel_bg: u32,
}

impl Tokens {
    /// Derive the full token set from a theme palette snapshot.
    ///
    /// Most roles map straight through from the palette so `/theme` re-themes
    /// everything; the default theme additionally retunes `muted`/`accent_dim`
    /// (legibility) and retones `you` to warm amber. Non-default themes keep the
    /// palette's own values (they are already internally consistent and, for
    /// `HighContrast`, deliberately maximal-contrast). When the palette carries
    /// all-zero colors (the `NO_COLOR` convention ⇒ "terminal default"),
    /// hierarchy is carried by [`Attr`] instead and every role stays `0`.
    #[must_use]
    pub fn from_palette(p: theme::Palette) -> Self {
        let is_default = p == theme::palette(theme::Theme::Default);
        // Retune only the default theme's legibility-critical roles; other
        // themes own consistent palettes already (Light/Dark/HighContrast).
        let (muted, accent_dim, you, band) = if is_default {
            (
                0x00_8A_84_7C, // retuned muted  — >=4.5:1 on bg + raised
                0x00_B8_89_5A, // retuned copper — >=4.5:1 on bg + raised
                0x00_E0_B8_78, // warm cream/amber — off cornflower-blue
                0x00_22_1D_18, // faint band tint, distinct from raised
            )
        } else {
            // Derive a band tint by nudging `raised` toward `surface` so it stays
            // theme-consistent without a new palette field.
            (p.muted, p.accent_dim, p.user, blend(p.surface_raised, p.surface))
        };
        Self {
            bg: p.surface,
            raised: p.surface_raised,
            band,
            accent: p.accent,
            accent_dim,
            muted,
            body: p.body,
            bright: p.bright,
            h1: p.h1,
            h2: p.h2,
            h3: p.h3,
            you,
            origin: p.accent,
            tool: p.tool,
            ok: p.green,
            warn: p.yellow,
            err: p.red,
            code_fg: p.code_fg,
            code_bg: p.code_bg,
            spine: p.accent,
            sel_bg: p.surface_raised,
        }
    }

    /// The default ("Burnished Copper") token set.
    #[must_use]
    pub fn default_tokens() -> Self {
        Self::from_palette(theme::palette(theme::Theme::Default))
    }

    /// The maximum-contrast token set (the `HighContrast` theme).
    #[must_use]
    pub fn high_contrast() -> Self {
        Self::from_palette(theme::palette(theme::Theme::HighContrast))
    }
}

impl Default for Tokens {
    fn default() -> Self {
        Self::default_tokens()
    }
}

/// Average two packed `0x00RRGGBB` colors channel-wise. Used to derive a `band`
/// tint for non-default themes without adding a palette field.
const fn blend(a: u32, b: u32) -> u32 {
    let ar = (a >> 16) & 0xFF;
    let ag = (a >> 8) & 0xFF;
    let ab = a & 0xFF;
    let br = (b >> 16) & 0xFF;
    let bg = (b >> 8) & 0xFF;
    let bb = b & 0xFF;
    let r = (ar + br) / 2;
    let g = (ag + bg) / 2;
    let bl = (ab + bb) / 2;
    (r << 16) | (g << 8) | bl
}

/// WCAG relative-contrast ratio between two packed `0x00RRGGBB` colors, in the
/// range `1.0..=21.0`. `>= 4.5` is the AA threshold for body text.
#[must_use]
pub fn contrast_ratio(fg: u32, bg: u32) -> f32 {
    let lf = relative_luminance(fg);
    let lb = relative_luminance(bg);
    let (hi, lo) = if lf >= lb { (lf, lb) } else { (lb, lf) };
    (hi + 0.05) / (lo + 0.05)
}

/// WCAG relative luminance of a packed `0x00RRGGBB` color (`0.0..=1.0`).
fn relative_luminance(c: u32) -> f32 {
    let chan = |shift: u32| {
        let byte = u8::try_from((c >> shift) & 0xFF).unwrap_or(0);
        let v = f32::from(byte) / 255.0;
        if v <= 0.039_28 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.0722_f32.mul_add(chan(0), 0.7152_f32.mul_add(chan(8), 0.2126 * chan(16)))
}

// ---------------------------------------------------------------------------
// Glyph family — the single icon set (Task F2)
// ---------------------------------------------------------------------------

/// The one glyph family. Retires the five competing status/role sets so the
/// whole UI draws from a single, consistent vocabulary (spec §"One glyph
/// family").
pub mod glyph {
    /// Wordmark / assistant role marker.
    pub const ORIGIN: char = '\u{25C6}'; // ◆
    /// Transcript spine gutter.
    pub const SPINE: char = '\u{2503}'; // ┃
                                        // Tool icons.
    pub const EDIT: char = '\u{270E}'; // ✎
    pub const BASH: char = '\u{2318}'; // ⌘
    pub const GREP: char = '\u{2315}'; // ⌕
    pub const READ: char = '\u{25C7}'; // ◇
    pub const WRITE: char = '\u{21F2}'; // ⇲
    pub const WEB: char = '\u{26BF}'; // ⚿
    pub const TASK: char = '\u{2295}'; // ⊕
                                       // Status.
    pub const RUN: char = '\u{25B8}'; // ▸
    pub const OK: char = '\u{2714}'; // ✔
    pub const FAIL: char = '\u{2718}'; // ✘
    pub const PERM: char = '\u{26A0}'; // ⚠
                                       // Composer / picker.
    pub const PROMPT: char = '\u{203A}'; // ›
    pub const CURSOR: char = '\u{25B8}'; // ▸
    pub const BOX_UNCHECKED: char = '\u{25A1}'; // □
    pub const BOX_CHECKED: char = '\u{25A0}'; // ■
    pub const CARET: char = '\u{258C}'; // ▌
    pub const QUOTE_BAR: char = '\u{258E}'; // ▎
}

/// A tool's icon + accent color, resolved from its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolToken {
    pub icon: char,
    pub fg: u32,
}

/// Map a tool name to its glyph + accent color (default theme accents).
///
/// Unknown tools fall back to the `◆` wordmark glyph in the base `tool` color.
/// Per-tool accents are derived here so the whole UI tints tools consistently.
#[must_use]
pub fn tool_token(name: &str) -> ToolToken {
    let t = Tokens::default_tokens();
    let lower = name.to_ascii_lowercase();
    let (icon, fg) = match lower.as_str() {
        "edit" => (glyph::EDIT, t.accent),
        "bash" | "shell" => (glyph::BASH, t.tool),
        "grep" | "search" => (glyph::GREP, t.warn),
        "read" => (glyph::READ, t.body),
        "write" => (glyph::WRITE, t.ok),
        "web_fetch" | "web" | "websearch" | "web_search" => (glyph::WEB, t.accent_dim),
        "task" | "agent" => (glyph::TASK, t.tool),
        _ => (glyph::ORIGIN, t.tool),
    };
    ToolToken { icon, fg }
}

// ---------------------------------------------------------------------------
// Region / RenderRow / RowSpan + blit_row  (Task F1)
// ---------------------------------------------------------------------------

/// A renderer-agnostic rectangle. Painters receive a `Region` and clip their
/// output to it; the orchestrator (`mod.rs`) owns absolute placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub top: u16,
    pub left: u16,
    pub width: u16,
    pub height: u16,
}

impl Region {
    /// Construct a region from its origin and extent.
    #[must_use]
    pub const fn new(top: u16, left: u16, width: u16, height: u16) -> Self {
        Self {
            top,
            left,
            width,
            height,
        }
    }

    /// Last column inside the region (`left + width`, exclusive bound).
    #[must_use]
    pub const fn right(self) -> u16 {
        self.left.saturating_add(self.width)
    }

    /// Last row inside the region (`top + height`, exclusive bound).
    #[must_use]
    pub const fn bottom(self) -> u16 {
        self.top.saturating_add(self.height)
    }
}

/// One owned, styled visual row a painter emits.
///
/// `indent` shifts the whole row right when blitted (hang-indent / nesting);
/// `spans` are written left to right after the indent. Decouples painters from
/// absolute grid coordinates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderRow {
    pub spans: Vec<RowSpan>,
    pub indent: u16,
}

impl RenderRow {
    /// An empty row (blank line) with no indent.
    #[must_use]
    pub fn blank() -> Self {
        Self::default()
    }

    /// A single-span row at zero indent.
    #[must_use]
    pub fn one(span: RowSpan) -> Self {
        Self {
            spans: vec![span],
            indent: 0,
        }
    }
}

/// A run of text sharing one style, inside a [`RenderRow`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowSpan {
    pub text: String,
    pub fg: u32,
    pub bg: u32,
    pub attr: Attr,
}

impl RowSpan {
    /// Construct a styled span.
    #[must_use]
    pub fn new(text: impl Into<String>, fg: u32, bg: u32, attr: Attr) -> Self {
        Self {
            text: text.into(),
            fg,
            bg,
            attr,
        }
    }

    /// A plain-attribute span (most common case).
    #[must_use]
    pub fn plain(text: impl Into<String>, fg: u32, bg: u32) -> Self {
        Self::new(text, fg, bg, Attr::PLAIN)
    }
}

/// Display width of one char in terminal cells. Control chars are zero-width
/// (emitting one raw would move the terminal cursor and corrupt the frame).
///
/// Mirrors `mod.rs::char_cell_width`; kept here so painters/`blit_row` are
/// self-contained and unit-testable without the parent module.
#[must_use]
pub fn char_cell_width(c: char) -> u16 {
    use unicode_width::UnicodeWidthChar;
    if c.is_control() {
        return 0;
    }
    u16::try_from(UnicodeWidthChar::width(c).unwrap_or(1)).unwrap_or(1)
}

/// Blit one [`RenderRow`] into `grid` on absolute `row`, starting at
/// `base_col + r.indent`, clipping at `max_cols`.
///
/// Wide glyphs (width 2) write a [`Cell::continuation`] in the trailing column
/// and are skipped entirely if only one column remains before `max_cols`. When
/// a span carries a non-default `bg`, the background is filled to `max_cols`
/// after the row's text so the band reads as a solid block (matching the
/// existing `render_md_line` bg-fill behavior).
pub fn blit_row(grid: &mut Grid, row: u16, base_col: u16, max_cols: u16, r: &RenderRow) {
    let mut col = base_col.saturating_add(r.indent);
    // The bg to fill to `max_cols` after the row, taken from the last span that
    // carried a non-default background. `0` ⇒ no fill (transparent row).
    let mut trailing_bg: u32 = 0;
    for span in &r.spans {
        if span.bg != 0 {
            trailing_bg = span.bg;
        }
        for ch in span.text.chars() {
            if col >= max_cols {
                return_fill(grid, row, col, max_cols, trailing_bg);
                return;
            }
            let w = char_cell_width(ch);
            if w == 0 {
                // Zero-width control: skip without advancing (never emit raw).
                continue;
            }
            if col + w > max_cols {
                // A wide glyph that would straddle the clip boundary: stop and
                // fill the remaining single column with bg rather than splitting
                // the glyph across the edge.
                return_fill(grid, row, col, max_cols, trailing_bg);
                return;
            }
            grid.put(row, col, Cell::new(ch, span.fg, span.bg, span.attr));
            if w == 2 {
                grid.put(row, col + 1, Cell::continuation(span.bg));
            }
            col += w;
        }
    }
    return_fill(grid, row, col, max_cols, trailing_bg);
}

/// Fill `[col, max_cols)` on `row` with blank cells in `bg` (no-op when
/// `bg == 0`, i.e. a transparent row).
fn return_fill(grid: &mut Grid, row: u16, mut col: u16, max_cols: u16, bg: u32) {
    if bg == 0 {
        return;
    }
    while col < max_cols {
        grid.put(row, col, Cell::new(' ', 0, bg, Attr::PLAIN));
        col += 1;
    }
}

#[cfg(test)]
mod token_tests {
    use super::*;

    #[test]
    fn text_tokens_clear_aa_contrast_on_both_surfaces() {
        // Every text role must clear the WCAG AA 4.5:1 threshold on both the
        // base bg and the raised surface, or the dim-on-dim legibility bug
        // returns. muted/accent_dim are retuned in from_palette to pass this.
        let t = Tokens::default_tokens();
        for (name, fg) in [
            ("muted", t.muted),
            ("body", t.body),
            ("bright", t.bright),
            ("accent_dim", t.accent_dim),
        ] {
            let on_bg = contrast_ratio(fg, t.bg);
            let on_raised = contrast_ratio(fg, t.raised);
            assert!(on_bg >= 4.5, "{name} on bg must be >=4.5:1, got {on_bg:.2}");
            assert!(
                on_raised >= 4.5,
                "{name} on raised must be >=4.5:1, got {on_raised:.2}"
            );
        }
    }

    #[test]
    fn retuned_values_are_exactly_the_spec_locked_hexes() {
        // Pin the retuned values so a later edit can't silently drift them back
        // under the contrast threshold (these are the documented contracts).
        let t = Tokens::default_tokens();
        assert_eq!(t.muted, 0x00_8A_84_7C, "retuned muted");
        assert_eq!(t.accent_dim, 0x00_B8_89_5A, "retuned accent_dim");
        assert_eq!(t.you, 0x00_E0_B8_78, "warm-amber you");
    }

    #[test]
    fn contrast_ratio_bounds_and_known_values() {
        // Identical colors ⇒ exactly 1.0; black-on-white ⇒ 21.0 (the extremes).
        assert!((contrast_ratio(0x00_80_80_80, 0x00_80_80_80) - 1.0).abs() < 1e-4);
        let bw = contrast_ratio(0x00_00_00_00, 0x00_FF_FF_FF);
        assert!((bw - 21.0).abs() < 0.1, "black/white ~21:1, got {bw:.2}");
    }

    #[test]
    fn default_surfaces_match_spec() {
        let t = Tokens::default_tokens();
        assert_eq!(t.bg, 0x00_0F_0D_0B);
        assert_eq!(t.raised, 0x00_1A_17_14);
        assert_eq!(t.accent, 0x00_D4_88_4E);
        assert_eq!(t.body, 0x00_C8_C1_B8);
        assert_eq!(t.bright, 0x00_F0_EB_E3);
        assert_eq!(t.origin, t.accent, "origin role == accent");
        assert_eq!(t.spine, t.accent, "spine == accent");
    }

    #[test]
    fn high_contrast_maps_through_and_stays_distinct() {
        // HighContrast must derive its (maximal-contrast) palette unchanged, so
        // NO_COLOR-style a11y is preserved and it differs from the default.
        let hc = Tokens::high_contrast();
        let hp = theme::palette(theme::Theme::HighContrast);
        assert_eq!(hc.bg, hp.surface);
        assert_eq!(hc.accent, hp.accent);
        assert_eq!(hc.body, hp.body);
        // HighContrast keeps the palette's own muted (not the default retune).
        assert_eq!(hc.muted, hp.muted);
        assert_ne!(
            hc.accent,
            Tokens::default_tokens().accent,
            "distinct from default"
        );
    }

    #[test]
    fn glyph_family_consts_are_the_spec_glyphs() {
        assert_eq!(glyph::ORIGIN, '\u{25C6}');
        assert_eq!(glyph::SPINE, '\u{2503}');
        assert_eq!(glyph::PROMPT, '\u{203A}');
        assert_eq!(glyph::BOX_UNCHECKED, '\u{25A1}');
        assert_eq!(glyph::BOX_CHECKED, '\u{25A0}');
        assert_eq!(glyph::OK, '\u{2714}');
        assert_eq!(glyph::FAIL, '\u{2718}');
    }

    #[test]
    fn tool_token_maps_known_and_defaults_unknown() {
        assert_eq!(tool_token("edit").icon, glyph::EDIT);
        assert_eq!(tool_token("BASH").icon, glyph::BASH, "case-insensitive");
        assert_eq!(tool_token("grep").icon, glyph::GREP);
        assert_eq!(tool_token("read").icon, glyph::READ);
        assert_eq!(tool_token("write").icon, glyph::WRITE);
        assert_eq!(tool_token("web_fetch").icon, glyph::WEB);
        assert_eq!(tool_token("task").icon, glyph::TASK);
        // Unknown ⇒ wordmark glyph fallback.
        assert_eq!(tool_token("frobnicate").icon, glyph::ORIGIN);
    }
}

#[cfg(test)]
mod row_tests {
    use super::*;

    #[test]
    fn blit_row_clips_wide_glyph_at_boundary() {
        // "a你b": 'a'(1) '你'(2) 'b'(1). With max_cols = 2 the wide glyph would
        // straddle col 1..3, so it must be dropped (not split) and nothing past
        // the clip is written.
        let mut grid = Grid::new(4, 1);
        let row = RenderRow::one(RowSpan::plain("a\u{4f60}b", 0x00_FF_FF_FF, 0));
        blit_row(&mut grid, 0, 0, 2, &row);
        assert_eq!(grid.get(0, 0).glyph, 'a' as u32, "narrow glyph placed");
        // col 1 must NOT hold the wide glyph or its continuation — it stayed blank.
        assert_eq!(grid.get(0, 1).glyph, Cell::blank().glyph, "wide glyph clipped");
    }

    #[test]
    fn blit_row_writes_wide_continuation() {
        // '你' at col 0 occupies cols 0-1: a real glyph + a continuation cell.
        let mut grid = Grid::new(4, 1);
        let row = RenderRow::one(RowSpan::plain("\u{4f60}", 0x00_C0_C0_C0, 0));
        blit_row(&mut grid, 0, 0, 4, &row);
        assert_eq!(grid.get(0, 0).glyph, '\u{4f60}' as u32, "wide glyph placed");
        assert!(
            grid.get(0, 1).is_continuation(),
            "trailing half is a continuation"
        );
    }

    #[test]
    fn blit_row_respects_indent_and_base_col() {
        let mut grid = Grid::new(8, 1);
        let row = RenderRow {
            spans: vec![RowSpan::plain("x", 0x00_FF_FF_FF, 0)],
            indent: 2,
        };
        blit_row(&mut grid, 0, 1, 8, &row);
        // base_col(1) + indent(2) = col 3.
        assert_eq!(grid.get(0, 3).glyph, 'x' as u32, "indent + base_col honored");
    }

    #[test]
    fn blit_row_fills_band_bg_to_max_cols() {
        // A span with a non-default bg fills its background to max_cols so a
        // code band reads as a solid block.
        let mut grid = Grid::new(6, 1);
        let bg = 0x00_16_13_11;
        let row = RenderRow::one(RowSpan::plain("ab", 0x00_FF_FF_FF, bg));
        blit_row(&mut grid, 0, 0, 6, &row);
        assert_eq!(grid.get(0, 2).bg, bg, "bg filled past the text");
        assert_eq!(grid.get(0, 5).bg, bg, "bg filled to max_cols");
    }
}
