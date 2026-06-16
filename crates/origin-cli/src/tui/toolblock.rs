// SPDX-License-Identifier: Apache-2.0
//! Contained tool-call block: header (icon + target + metrics), nested body /
//! diff, line-count collapse.
//!
//! Wave 0 stub: locks `ToolView`/`ToolStatus`/`ToolBody`/`DiffLine`/`DiffKind`
//! and the `layout_tool`/`diff_gutter` signatures. Wave 1 (Task G) fills the
//! bodies. Field names are derived from the tool-activity state `mod.rs`
//! tracks (name, target, running/ok/fail, +/- counts, elapsed, output body).

// The stub bodies look const-foldable now; Wave 1 fills them with real
// (non-const) layout logic, so they are deliberately left non-const.
#![allow(dead_code, clippy::missing_const_for_fn)] // Wave 1 fills this

use super::tokens::{RenderRow, Tokens};

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

/// Lay out a tool call into a contained block: header row (icon + target +
/// right-aligned `+N −N · elapsed · ✔/✘`) plus the nested body, collapsing long
/// output by line count.
#[must_use]
pub fn layout_tool(_call: &ToolView, _width: u16, _tok: &Tokens) -> Vec<RenderRow> {
    // Wave 1 (Task G) fills this.
    Vec::new()
}

/// Build the diff gutter glyph + style for one diff-line kind.
#[must_use]
pub fn diff_gutter(_kind: DiffKind, _tok: &Tokens) -> (char, u32) {
    // Wave 1 (Task G) fills this.
    (' ', 0)
}
