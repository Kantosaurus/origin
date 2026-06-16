// SPDX-License-Identifier: Apache-2.0
//! Contained tool-call block: header (icon + target + metrics), nested body /
//! diff, line-count collapse.
//!
//! Wave 0 stub: locks `ToolView`/`ToolStatus`/`ToolBody`/`DiffLine`/`DiffKind`
//! and the `layout_tool`/`diff_gutter` signatures. Wave 1 (Task G) fills the
//! bodies. Field names are derived from the tool-activity state `mod.rs`
//! tracks (name, target, running/ok/fail, +/- counts, elapsed, output body).

// `layout_tool`/helpers are consumed by `mod.rs` in Wave 2; until then the
// public entry is only exercised by this module's tests, so the private helpers
// read as dead code at the crate level. `diff_gutter`/`status_token` are pure
// lookups that *could* be const, but the locked stub deliberately keeps the
// layout entry points non-const (Wave-2 may add non-const wiring), so the
// `missing_const_for_fn` lint is silenced to honor the signature contract.
#![allow(dead_code, clippy::missing_const_for_fn)]

use origin_tui::grid::Attr;

use super::tokens::{glyph, tool_token, RenderRow, RowSpan, Tokens};

/// Maximum number of body rows shown before the block collapses the tail into a
/// single muted "… +N more lines" summary row (collapse is **by line count**,
/// not bytes — see the spec's "Tool-call blocks" section).
const BODY_ROW_CAP: usize = 12;

/// Left connector glyph that ties the nested body to the spine, mirroring the
/// header's tool icon column.
const CONNECTOR: char = '\u{2502}'; // │

/// A tool call ready to render as a contained block.
#[derive(Debug, Clone)]
pub struct ToolView {
    /// Tool name (resolves to icon + accent via `tokens::tool_token`).
    pub name: String,
    /// What the call acts on (path, command, query), shown in the header.
    pub target: String,
    /// Lifecycle state, driving the `▸`/`✔`/`✘` marker.
    pub status: ToolStatus,
    /// Lines added (diff metric), `0` when not a diff.
    pub added: u32,
    /// Lines removed (diff metric), `0` when not a diff.
    pub removed: u32,
    /// Wall-clock duration in milliseconds, shown right-aligned.
    pub elapsed_ms: u64,
    /// The nested body beneath the header.
    pub body: ToolBody,
}

/// The lifecycle state of a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Ok,
    Fail,
}

/// The nested content under a tool header.
#[derive(Debug, Clone)]
pub enum ToolBody {
    /// Plain output lines (e.g. streamed bash).
    Text(Vec<String>),
    /// A unified diff (edits/writes).
    Diff(Vec<DiffLine>),
    /// A file read, with a starting line number and the read lines.
    Read {
        path: String,
        start: u32,
        lines: Vec<String>,
    },
}

impl Default for ToolBody {
    fn default() -> Self {
        Self::Text(Vec::new())
    }
}

/// One line of a rendered diff body.
#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
}

/// The kind of a diff line, driving the gutter glyph + color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Add,
    Del,
    Ctx,
}

/// Format a duration in milliseconds compactly: `<1s` as `NNNms`, `>=1s` as one
/// decimal place of seconds (e.g. `340ms`, `1.2s`, `12.0s`).
#[must_use]
fn format_elapsed(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        // Durations shown here are small (a few minutes at most); the f64 cast
        // can't lose meaningful precision for a 1-decimal seconds readout.
        #[allow(clippy::cast_precision_loss)]
        let secs = ms as f64 / 1000.0;
        format!("{secs:.1}s")
    }
}

/// The status glyph + color shown at the tail of the header metrics. `Running`
/// shows the `▸` run marker; `Ok`/`Fail` show `✔`/`✘`.
#[must_use]
fn status_token(status: ToolStatus, tok: &Tokens) -> (char, u32) {
    match status {
        ToolStatus::Running => (glyph::RUN, tok.accent),
        ToolStatus::Ok => (glyph::OK, tok.ok),
        ToolStatus::Fail => (glyph::FAIL, tok.err),
    }
}

/// Build the diff gutter glyph + style for one diff-line kind.
///
/// `Add` ⇒ `+` (ok/green), `Del` ⇒ `−` (err/red, the U+2212 minus sign so it
/// reads as a true diff gutter rather than a hyphen), `Ctx` ⇒ `·` (muted).
#[must_use]
pub fn diff_gutter(kind: DiffKind, tok: &Tokens) -> (char, u32) {
    match kind {
        DiffKind::Add => ('+', tok.ok),
        DiffKind::Del => ('\u{2212}', tok.err), // −
        DiffKind::Ctx => ('\u{00B7}', tok.muted), // ·
    }
}

/// Build the right-aligned metric string: `+A −R · {elapsed} · {status}`.
///
/// The `+A −R` diff counts are omitted when both are `0` (non-diff tools),
/// keeping the metric compact. Returned without the trailing status glyph (that
/// is appended as its own colored span so it keeps its semantic color).
#[must_use]
fn metric_prefix(call: &ToolView) -> String {
    let mut parts: Vec<String> = Vec::new();
    if call.added > 0 || call.removed > 0 {
        parts.push(format!("+{} \u{2212}{}", call.added, call.removed));
    }
    parts.push(format_elapsed(call.elapsed_ms));
    // Joined with " · "; the status glyph is appended by the caller.
    let joined = parts.join(" \u{00B7} ");
    format!("{joined} \u{00B7} ")
}

/// Lay out a tool call into a contained block: header row (icon + target +
/// right-aligned `+N −N · elapsed · ✔/✘`) plus the nested body, collapsing long
/// output by line count.
#[must_use]
pub fn layout_tool(call: &ToolView, width: u16, tok: &Tokens) -> Vec<RenderRow> {
    let mut rows: Vec<RenderRow> = Vec::new();
    let cols = width as usize;

    // --- Header row -------------------------------------------------------
    let token = tool_token(&call.name);
    let (status_glyph, status_fg) = status_token(call.status, tok);
    let metric = metric_prefix(call);

    // Left side: "icon target".
    let icon = format!("{} ", token.icon);
    let left_text = format!("{icon}{}", call.target);
    // Right side width = metric prefix + the 1-cell status glyph.
    let right_width = display_width(&metric) + display_width(&status_glyph.to_string());

    let mut header_spans = vec![
        RowSpan::new(icon, token.fg, 0, Attr::BOLD),
        RowSpan::plain(call.target.clone(), tok.body, 0),
    ];

    // Compute padding so the metric is flush to `width`. If the left text plus
    // the metric don't fit, drop the padding (they butt with a single space).
    let left_width = display_width(&left_text);
    let pad = cols
        .saturating_sub(left_width)
        .saturating_sub(right_width)
        .max(1);
    header_spans.push(RowSpan::plain(" ".repeat(pad), tok.muted, 0));
    header_spans.push(RowSpan::plain(metric, tok.muted, 0));
    header_spans.push(RowSpan::new(status_glyph.to_string(), status_fg, 0, Attr::BOLD));
    rows.push(RenderRow {
        spans: header_spans,
        indent: 0,
    });

    // --- Body -------------------------------------------------------------
    match &call.body {
        ToolBody::Text(lines) => layout_text(lines, tok, &mut rows),
        ToolBody::Diff(lines) => layout_diff(lines, tok, &mut rows),
        ToolBody::Read { start, lines, .. } => layout_read(*start, lines, tok, &mut rows),
    }

    rows
}

/// Indent under the header so the body nests beneath the tool icon column,
/// prefixed with the `│` connector span. Returns the connector span + the body
/// span placed after it.
fn body_row(connector_fg: u32, body: RowSpan) -> RenderRow {
    RenderRow {
        spans: vec![
            RowSpan::plain(format!("{CONNECTOR} "), connector_fg, 0),
            body,
        ],
        indent: 0,
    }
}

/// A muted "… +N more lines" collapse row, mirroring `diff_elision_summary`'s
/// wording (the 2-space lead nests it under the header).
fn elision_row(hidden: usize, tok: &Tokens) -> RenderRow {
    body_row(
        tok.muted,
        RowSpan::plain(format!("\u{2026} +{hidden} more lines"), tok.muted, 0),
    )
}

/// Render plain text output lines (bash, etc.), collapsing past the cap.
fn layout_text(lines: &[String], tok: &Tokens, rows: &mut Vec<RenderRow>) {
    let shown = collapse_count(lines.len());
    for line in &lines[..shown] {
        rows.push(body_row(tok.spine, RowSpan::plain(line.clone(), tok.body, 0)));
    }
    if let Some(hidden) = hidden_count(lines.len(), shown) {
        rows.push(elision_row(hidden, tok));
    }
}

/// Render a diff body with the per-kind gutter glyph + color, collapsing past
/// the cap.
fn layout_diff(lines: &[DiffLine], tok: &Tokens, rows: &mut Vec<RenderRow>) {
    let shown = collapse_count(lines.len());
    for dl in &lines[..shown] {
        let (g, fg) = diff_gutter(dl.kind, tok);
        let text_fg = match dl.kind {
            DiffKind::Add => tok.bright,
            DiffKind::Del => tok.muted,
            DiffKind::Ctx => tok.body,
        };
        rows.push(RenderRow {
            spans: vec![
                RowSpan::plain(format!("{CONNECTOR} "), tok.spine, 0),
                RowSpan::plain(format!("{g} "), fg, 0),
                RowSpan::plain(dl.text.clone(), text_fg, 0),
            ],
            indent: 0,
        });
    }
    if let Some(hidden) = hidden_count(lines.len(), shown) {
        rows.push(elision_row(hidden, tok));
    }
}

/// Render a file read with right-aligned line numbers counted from `start`,
/// collapsing past the cap.
fn layout_read(start: u32, lines: &[String], tok: &Tokens, rows: &mut Vec<RenderRow>) {
    let shown = collapse_count(lines.len());
    // Width of the largest line number shown drives right-alignment.
    let last_no = u64::from(start) + (shown.saturating_sub(1)) as u64;
    let num_width = last_no.to_string().len();
    for (i, line) in lines[..shown].iter().enumerate() {
        let no = u64::from(start) + i as u64;
        let gutter = format!("{no:>num_width$} ");
        rows.push(RenderRow {
            spans: vec![
                RowSpan::plain(format!("{CONNECTOR} "), tok.spine, 0),
                RowSpan::plain(gutter, tok.muted, 0),
                RowSpan::plain(line.clone(), tok.body, 0),
            ],
            indent: 0,
        });
    }
    if let Some(hidden) = hidden_count(lines.len(), shown) {
        rows.push(elision_row(hidden, tok));
    }
}

/// How many of `total` body rows to actually render before collapsing.
const fn collapse_count(total: usize) -> usize {
    if total > BODY_ROW_CAP {
        BODY_ROW_CAP
    } else {
        total
    }
}

/// The hidden-row count for the elision summary, or `None` when nothing was
/// collapsed.
const fn hidden_count(total: usize, shown: usize) -> Option<usize> {
    if total > shown {
        Some(total - shown)
    } else {
        None
    }
}

/// Display width (in terminal cells) of a string, summing per-char cell widths.
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| super::tokens::char_cell_width(c) as usize)
        .sum()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tok() -> Tokens {
        Tokens::default_tokens()
    }

    /// Total display width of a `RenderRow` (indent + all span widths).
    fn row_width(row: &RenderRow) -> usize {
        row.indent as usize
            + row
                .spans
                .iter()
                .map(|s| display_width(&s.text))
                .sum::<usize>()
    }

    /// Concatenated text of a `RenderRow`.
    fn row_text(row: &RenderRow) -> String {
        row.spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn format_elapsed_is_compact() {
        assert_eq!(format_elapsed(0), "0ms");
        assert_eq!(format_elapsed(340), "340ms");
        assert_eq!(format_elapsed(999), "999ms");
        assert_eq!(format_elapsed(1000), "1.0s");
        assert_eq!(format_elapsed(1200), "1.2s");
        assert_eq!(format_elapsed(12_000), "12.0s");
    }

    #[test]
    fn header_metric_includes_counts_elapsed_and_status() {
        let call = ToolView {
            name: "edit".into(),
            target: "src/lib.rs".into(),
            status: ToolStatus::Ok,
            added: 3,
            removed: 1,
            elapsed_ms: 1200,
            body: ToolBody::Diff(Vec::new()),
        };
        let rows = layout_tool(&call, 80, &tok());
        let header = row_text(&rows[0]);
        assert!(header.contains("+3 \u{2212}1"), "diff counts: {header}");
        assert!(header.contains("1.2s"), "elapsed: {header}");
        assert!(header.contains(glyph::OK), "ok status glyph: {header}");
        assert!(header.contains("src/lib.rs"), "target: {header}");
    }

    #[test]
    fn header_omits_counts_for_non_diff_tools() {
        let call = ToolView {
            name: "bash".into(),
            target: "ls -la".into(),
            status: ToolStatus::Ok,
            added: 0,
            removed: 0,
            elapsed_ms: 50,
            body: ToolBody::Text(vec!["a".into()]),
        };
        let rows = layout_tool(&call, 80, &tok());
        let header = row_text(&rows[0]);
        assert!(!header.contains('+'), "no diff counts shown: {header}");
        assert!(header.contains("50ms"), "elapsed still shown: {header}");
    }

    #[test]
    fn running_shows_run_marker_not_check() {
        let call = ToolView {
            name: "bash".into(),
            target: "sleep 1".into(),
            status: ToolStatus::Running,
            added: 0,
            removed: 0,
            elapsed_ms: 500,
            body: ToolBody::Text(Vec::new()),
        };
        let rows = layout_tool(&call, 60, &tok());
        let header = row_text(&rows[0]);
        assert!(header.contains(glyph::RUN), "running ▸ marker: {header}");
        assert!(!header.contains(glyph::OK), "no ✔ while running: {header}");
        assert!(!header.contains(glyph::FAIL), "no ✘ while running: {header}");
    }

    #[test]
    fn header_metric_right_aligned_within_width() {
        let call = ToolView {
            name: "edit".into(),
            target: "a.rs".into(),
            status: ToolStatus::Ok,
            added: 2,
            removed: 2,
            elapsed_ms: 900,
            body: ToolBody::Diff(Vec::new()),
        };
        let width = 50;
        let rows = layout_tool(&call, width, &tok());
        let header = &rows[0];
        // The header must not overflow the available width.
        assert!(
            row_width(header) <= width as usize,
            "header width {} exceeds {width}",
            row_width(header)
        );
        // And it must (nearly) fill the width — the metric is flush right, so
        // the row width equals `width` exactly when padding is present.
        assert_eq!(row_width(header), width as usize, "metric is flush-right");
    }

    #[test]
    fn diff_gutter_glyph_and_color_per_kind() {
        let t = tok();
        assert_eq!(diff_gutter(DiffKind::Add, &t), ('+', t.ok));
        assert_eq!(diff_gutter(DiffKind::Del, &t), ('\u{2212}', t.err));
        assert_eq!(diff_gutter(DiffKind::Ctx, &t), ('\u{00B7}', t.muted));
    }

    #[test]
    fn diff_body_uses_gutter_glyphs() {
        let call = ToolView {
            name: "edit".into(),
            target: "x.rs".into(),
            status: ToolStatus::Ok,
            added: 1,
            removed: 1,
            elapsed_ms: 10,
            body: ToolBody::Diff(vec![
                DiffLine { kind: DiffKind::Ctx, text: "fn main() {".into() },
                DiffLine { kind: DiffKind::Del, text: "    old();".into() },
                DiffLine { kind: DiffKind::Add, text: "    new();".into() },
            ]),
        };
        let rows = layout_tool(&call, 80, &tok());
        // rows[0] = header; rows[1..] = body.
        assert!(row_text(&rows[2]).contains('\u{2212}'), "del gutter −");
        assert!(row_text(&rows[3]).contains('+'), "add gutter +");
        assert!(row_text(&rows[1]).contains('\u{00B7}'), "ctx gutter ·");
    }

    #[test]
    fn collapse_emits_elision_row_at_right_count() {
        // 20 text lines, cap 12 ⇒ 12 body rows + 1 elision row mentioning 8.
        let lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        let call = ToolView {
            name: "bash".into(),
            target: "cat big".into(),
            status: ToolStatus::Ok,
            added: 0,
            removed: 0,
            elapsed_ms: 5,
            body: ToolBody::Text(lines),
        };
        let rows = layout_tool(&call, 80, &tok());
        // 1 header + 12 shown + 1 elision = 14.
        assert_eq!(rows.len(), 1 + BODY_ROW_CAP + 1);
        let last = row_text(rows.last().unwrap());
        assert!(last.contains("+8 more lines"), "hidden count 8: {last}");
        assert!(last.contains('\u{2026}'), "ellipsis glyph: {last}");
    }

    #[test]
    fn no_elision_when_under_cap() {
        let lines: Vec<String> = (0..5).map(|i| format!("line {i}")).collect();
        let call = ToolView {
            name: "bash".into(),
            target: "cat small".into(),
            status: ToolStatus::Ok,
            added: 0,
            removed: 0,
            elapsed_ms: 5,
            body: ToolBody::Text(lines),
        };
        let rows = layout_tool(&call, 80, &tok());
        assert_eq!(rows.len(), 1 + 5, "header + 5 body, no elision");
        for r in &rows {
            assert!(!row_text(r).contains("more lines"), "no collapse row");
        }
    }

    #[test]
    fn read_line_numbers_right_aligned() {
        // start=8, 3 lines ⇒ numbers 8,9,10. The single-digit 8/9 must be
        // right-padded to the 2-digit width of 10.
        let call = ToolView {
            name: "read".into(),
            target: "src/main.rs".into(),
            status: ToolStatus::Ok,
            added: 0,
            removed: 0,
            elapsed_ms: 3,
            body: ToolBody::Read {
                path: "src/main.rs".into(),
                start: 8,
                lines: vec!["a".into(), "b".into(), "c".into()],
            },
        };
        let rows = layout_tool(&call, 80, &tok());
        // The gutter is the 2nd span of each body row: "{num:>2} ".
        let g0 = rows[1].spans[1].text.clone();
        let g1 = rows[2].spans[1].text.clone();
        let g2 = rows[3].spans[1].text.clone();
        assert_eq!(g0, " 8 ", "8 right-aligned to width 2");
        assert_eq!(g1, " 9 ", "9 right-aligned to width 2");
        assert_eq!(g2, "10 ", "10 sets the width");
        // All gutters share the same display width.
        assert_eq!(display_width(&g0), display_width(&g2), "aligned widths");
    }

    #[test]
    fn read_collapses_and_counts_from_start() {
        let lines: Vec<String> = (0..30).map(|i| format!("l{i}")).collect();
        let call = ToolView {
            name: "read".into(),
            target: "big.txt".into(),
            status: ToolStatus::Ok,
            added: 0,
            removed: 0,
            elapsed_ms: 3,
            body: ToolBody::Read {
                path: "big.txt".into(),
                start: 100,
                lines,
            },
        };
        let rows = layout_tool(&call, 80, &tok());
        assert_eq!(rows.len(), 1 + BODY_ROW_CAP + 1);
        // First shown line number is `start`.
        assert!(rows[1].spans[1].text.trim() == "100", "counts from start");
        let last = row_text(rows.last().unwrap());
        assert!(last.contains("+18 more lines"), "30-12=18 hidden: {last}");
    }
}
