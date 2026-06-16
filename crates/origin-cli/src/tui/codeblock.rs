// SPDX-License-Identifier: Apache-2.0
//! Fenced code-block framing: language label + left rule + band background,
//! tinted via [`super::syntax`].
//!
//! `layout_code` turns the (fence-stripped) lines of a fenced code block into
//! framed [`RenderRow`]s: a dim language-label row, then each source line behind
//! a `▎` left rule (in `accent_dim`) on a `band` background, lexically tinted via
//! [`super::syntax::tint`]. The literal ` ``` ` fences the caller stripped are
//! replaced by this clean framing.

// The painters here are consumed by the Wave-2 `mod.rs` draw orchestration,
// which is being wired in parallel — until then they read as dead code.
#![allow(dead_code)] // Wave 2 wires these into the draw flow.

use super::syntax::{self, Tok};
use super::tokens::{glyph, RenderRow, RowSpan, Tokens};
use origin_tui::grid::Attr;

/// Columns the left rule (`▎` + a space) occupies before each code line.
const RULE_COLS: u16 = 2;

/// Map a lexical [`Tok`] class to its token color in the current theme.
const fn tok_color(kind: Tok, tok: &Tokens) -> u32 {
    match kind {
        Tok::Keyword => tok.accent,
        Tok::Str => tok.ok,
        Tok::Comment => tok.muted,
        Tok::Num => tok.warn,
        Tok::Ident => tok.code_fg,
        Tok::Punct => tok.body,
    }
}

/// Build the dim language-label row that heads a code block. Shows the language
/// label (or `code` when unlabeled) behind the same left rule as the body, so
/// the block reads as one framed unit.
fn label_row(lang: &str, tok: &Tokens) -> RenderRow {
    let label = if lang.is_empty() { "code" } else { lang };
    RenderRow {
        spans: vec![
            RowSpan::new(
                format!("{} ", glyph::QUOTE_BAR),
                tok.accent_dim,
                tok.band,
                Attr::PLAIN,
            ),
            RowSpan::new(label.to_string(), tok.muted, tok.band, Attr::DIM),
        ],
        indent: 0,
    }
}

/// Lay out one source line into spans: the left rule, then the line tinted by
/// `spans` (byte ranges from [`syntax::tint`]). Untinted gaps fall back to
/// `code_fg`. The whole row sits on the `band` background.
fn code_row(line: &str, spans: &[syntax::Span], width: u16, tok: &Tokens) -> RenderRow {
    let mut out: Vec<RowSpan> = vec![RowSpan::new(
        format!("{} ", glyph::QUOTE_BAR),
        tok.accent_dim,
        tok.band,
        Attr::PLAIN,
    )];

    // Clip the visible source to the columns left after the rule. `width` is the
    // full block width; the rule eats `RULE_COLS`. We clip by chars (the blitter
    // also clips, but trimming here keeps spans tidy).
    let avail = usize::from(width.saturating_sub(RULE_COLS));
    let truncated: String = line.chars().take(avail.max(1)).collect();
    let trunc_bytes = truncated.len();

    // Walk the byte range, emitting a tinted span where one covers the cursor and
    // a default-colored span for the gaps. Spans from `tint` are non-overlapping
    // and in-range, but we still clamp to `trunc_bytes` for the clipped tail.
    let mut cursor = 0usize;
    for sp in spans {
        if sp.start >= trunc_bytes {
            break;
        }
        if sp.start > cursor {
            // Untinted gap.
            push_slice(&mut out, &truncated, cursor, sp.start, tok.code_fg, tok);
        }
        let end = (sp.start + sp.len).min(trunc_bytes);
        push_slice(&mut out, &truncated, sp.start, end, tok_color(sp.kind, tok), tok);
        cursor = end;
    }
    if cursor < trunc_bytes {
        push_slice(&mut out, &truncated, cursor, trunc_bytes, tok.code_fg, tok);
    }

    RenderRow {
        spans: out,
        indent: 0,
    }
}

/// Push `s[from..to]` as a span in `fg` on the band background, if non-empty and
/// on a valid char boundary (defensive — `tint` already yields char boundaries).
fn push_slice(out: &mut Vec<RowSpan>, s: &str, from: usize, to: usize, fg: u32, tok: &Tokens) {
    if to <= from {
        return;
    }
    let Some(text) = s.get(from..to) else {
        return;
    };
    if text.is_empty() {
        return;
    }
    out.push(RowSpan::new(text.to_string(), fg, tok.band, Attr::PLAIN));
}

/// Lay out a fenced code block (its lines, already fence-stripped) into framed,
/// tinted [`RenderRow`]s for `lang`, clipped to `width`.
///
/// The result is `[label_row, body_row*]`: a dim language-label row followed by
/// one row per source line, each behind a `▎` left rule on the `band`
/// background. Lines are tinted via [`syntax::tint`] when `lang` resolves to a
/// known [`syntax::Lang`]; an unknown label yields untinted (but still framed)
/// rows. The literal fences are not emitted — this framing replaces them.
#[must_use]
pub fn layout_code(lines: &[&str], lang: &str, width: u16, tok: &Tokens) -> Vec<RenderRow> {
    let mut rows = Vec::with_capacity(lines.len() + 1);
    rows.push(label_row(lang, tok));

    let resolved = syntax::lang_from_label(lang);
    for line in lines {
        let spans = resolved.map_or_else(Vec::new, |l| syntax::tint(l, line));
        rows.push(code_row(line, &spans, width, tok));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok() -> Tokens {
        Tokens::default_tokens()
    }

    /// The first span of a row is always the left rule glyph + space.
    fn assert_rule(row: &RenderRow, tok: &Tokens) {
        let first = &row.spans[0];
        assert!(
            first.text.starts_with(glyph::QUOTE_BAR),
            "row starts with the ▎ left rule, got {:?}",
            first.text
        );
        assert_eq!(first.fg, tok.accent_dim, "left rule uses accent_dim");
        assert_eq!(first.bg, tok.band, "left rule on band bg");
    }

    #[test]
    fn emits_label_row_plus_one_row_per_line() {
        let t = tok();
        let lines = ["let x = 1;", "let y = 2;", "foo();"];
        let rows = layout_code(&lines, "rust", 40, &t);
        // 1 label row + 3 source rows.
        assert_eq!(rows.len(), 4, "label + one row per source line");
    }

    #[test]
    fn label_row_shows_language_and_band_bg() {
        let t = tok();
        let rows = layout_code(&["x"], "rust", 40, &t);
        let label = &rows[0];
        assert_rule(label, &t);
        // The label text "rust" appears in a muted span on the band bg.
        let label_span = &label.spans[1];
        assert_eq!(label_span.text, "rust");
        assert_eq!(label_span.fg, t.muted, "label is muted");
        assert_eq!(label_span.bg, t.band, "label on band bg");
        assert_eq!(label_span.attr, Attr::DIM, "label is dim");
    }

    #[test]
    fn unlabeled_block_labels_as_code() {
        let t = tok();
        let rows = layout_code(&["x = 1"], "", 40, &t);
        assert_eq!(rows[0].spans[1].text, "code", "empty lang ⇒ 'code' label");
    }

    #[test]
    fn body_rows_sit_on_the_band_background() {
        let t = tok();
        let rows = layout_code(&["plain line"], "unknownlang", 40, &t);
        assert_rule(&rows[1], &t);
        // Even with no tint (unknown lang), the source text is present on band bg.
        let text: String = rows[1].spans.iter().skip(1).map(|s| s.text.clone()).collect();
        assert_eq!(text, "plain line", "source text preserved");
        for span in rows[1].spans.iter().skip(1) {
            assert_eq!(span.bg, t.band, "body span on band bg");
        }
    }

    #[test]
    fn unknown_language_is_untinted_but_framed() {
        let t = tok();
        let rows = layout_code(&["whatever here"], "klingon", 40, &t);
        // No tint ⇒ a single code_fg span after the rule.
        let body_spans: Vec<_> = rows[1].spans.iter().skip(1).collect();
        assert!(!body_spans.is_empty());
        for s in body_spans {
            assert_eq!(s.fg, t.code_fg, "untinted lines use code_fg");
        }
    }

    #[test]
    fn empty_block_still_emits_a_label_row() {
        let t = tok();
        let rows = layout_code(&[], "py", 40, &t);
        assert_eq!(rows.len(), 1, "just the label row");
        assert_eq!(rows[0].spans[1].text, "py");
    }

    #[test]
    fn narrow_width_does_not_panic_and_clips() {
        let t = tok();
        // width 4: rule eats 2 cols, ~2 left for source.
        let rows = layout_code(&["a very long line of source"], "unknownlang", 4, &t);
        assert_eq!(rows.len(), 2);
        // Body text is clipped to the available columns (chars), not the full line.
        let text: String = rows[1].spans.iter().skip(1).map(|s| s.text.clone()).collect();
        assert!(text.len() <= 2, "clipped to available cols, got {text:?}");
    }

    #[test]
    fn tok_color_maps_each_kind_to_a_theme_color() {
        let t = tok();
        assert_eq!(tok_color(Tok::Keyword, &t), t.accent);
        assert_eq!(tok_color(Tok::Str, &t), t.ok);
        assert_eq!(tok_color(Tok::Comment, &t), t.muted);
        assert_eq!(tok_color(Tok::Num, &t), t.warn);
        assert_eq!(tok_color(Tok::Ident, &t), t.code_fg);
        assert_eq!(tok_color(Tok::Punct, &t), t.body);
    }

    #[test]
    fn tinted_span_paints_its_token_color() {
        // Drive `code_row` directly with a hand-built span so the assertion does
        // not depend on the parallel `syntax` lexer's real output: a Keyword span
        // over bytes 0..3 of "fnX" must paint in the keyword (accent) color.
        let t = tok();
        let spans = [syntax::Span { start: 0, len: 2, kind: Tok::Keyword }];
        let row = code_row("fn main", &spans, 40, &t);
        // spans[0] is the rule; spans[1] should be the keyword "fn" in accent.
        let kw = &row.spans[1];
        assert_eq!(kw.text, "fn");
        assert_eq!(kw.fg, t.accent, "keyword tinted with accent");
        assert_eq!(kw.bg, t.band, "tinted span still on band bg");
    }
}
