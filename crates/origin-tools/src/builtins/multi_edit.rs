// SPDX-License-Identifier: Apache-2.0
//! `MultiEdit` — apply a list of edit operations to one file, atomically.

use crate::builtins::editmatch;
use crate::builtins::write::WriteGuard;
use crate::error::{ErrClass, ToolError};
use crate::text_fmt;
use crate::{SideEffects, Tier, Urgency};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct EditOp {
    pub old: String,
    pub new: String,
    pub replace_all: bool,
}

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct MultiEditArgs {
    pub file_path: String,
    pub edits: Vec<EditOp>,
}

/// # Errors
/// `edit.no_match | edit.ambiguous | io.*`
pub fn multi_edit(args: &MultiEditArgs, guard: Option<&WriteGuard>) -> Result<Value, ToolError> {
    // Read-before-edit guard (mirrors Write/Edit): refuse to edit a file not Read
    // this session. `None` ⇒ no guard (tests/headless).
    if let Some(g) = guard {
        if !g.has_read(&args.file_path) {
            return Err(ToolError::new(
                ErrClass::Edit,
                "read_required",
                format!("refusing to MultiEdit '{}' that has not been Read this session", args.file_path),
            )
            .recoverable(true)
            .hint("call Read on this file first, then re-MultiEdit"));
        }
    }
    let bytes = std::fs::read(&args.file_path)
        .map_err(|e| ToolError::new(ErrClass::Io, "not_found", format!("{}: {e}", args.file_path)))?;
    let det = text_fmt::detect(&bytes);
    let original = text_fmt::normalise_to_lf(&bytes, &det)?;
    let total = args.edits.len();

    // Validate every op up front (cheap, fail-fast, no partial file writes).
    for (i, op) in args.edits.iter().enumerate() {
        if op.old.is_empty() {
            return Err(ToolError::new(
                ErrClass::Validation,
                "empty_old_string",
                format!("edit {} of {total}: old must not be empty", i + 1),
            )
            .recoverable(true));
        }
        if op.old == op.new {
            return Err(ToolError::new(
                ErrClass::Edit,
                "noop",
                format!("edit {} of {total}: new equals old; this edit changes nothing", i + 1),
            )
            .recoverable(true)
            .hint("remove the no-op edit or target a different region"));
        }
    }

    // C14: when every op resolves to a UNIQUE, DISJOINT range against the
    // original snapshot, apply them in one right-to-left pass — so no edit's
    // result can shift another's match (the order-dependent corruption class).
    // Anything that can't (replace_all, chained edits whose `old` only exists
    // after a prior edit, overlaps) falls back to the sequential apply below,
    // preserving today's chained-edit semantics.
    let text = if let Some(updated) = try_disjoint_apply(&original, &args.edits) {
        updated
    } else {
        sequential_apply(original, &args.edits, total)?
    };

    let new_bytes = text_fmt::denormalise(&text, &det);
    atomic_write(&args.file_path, &new_bytes)?;
    Ok(json!({"ok": true, "applied": total}))
}

type Resolved = (std::ops::Range<usize>, String);

/// Resolve every op to a unique byte range in the ORIGINAL `text` and, if all
/// ranges are disjoint, splice the replacements right-to-left. `None` (→ caller
/// falls back to [`sequential_apply`]) when any op uses `replace_all`, matches
/// 0 or >1 times in the original, or its range overlaps another's.
fn try_disjoint_apply(text: &str, edits: &[EditOp]) -> Option<String> {
    // (target byte range in `text`, re-indented replacement)
    let mut ranges: Vec<Resolved> = Vec::with_capacity(edits.len());
    for op in edits {
        if op.replace_all {
            return None;
        }
        let range = match text.matches(op.old.as_str()).count() {
            1 => {
                let start = text.find(op.old.as_str())?;
                start..start + op.old.len()
            }
            _ => editmatch::ws_tolerant_unique_range(text, &op.old)?,
        };
        let new = editmatch::reindent_replacement(&op.new, &op.old, &text[range.clone()]);
        ranges.push((range, new));
    }
    ranges.sort_by_key(|(r, _)| r.start);
    if ranges.windows(2).any(|w| w[0].0.end > w[1].0.start) {
        return None; // overlapping edits ⇒ not disjoint
    }
    let mut s = text.to_string();
    for (range, new) in ranges.into_iter().rev() {
        s.replace_range(range, &new);
    }
    Some(s)
}

/// The original chained-capable apply: each op runs against the buffer already
/// mutated by earlier ops. Used when [`try_disjoint_apply`] can't apply cleanly.
fn sequential_apply(mut text: String, edits: &[EditOp], total: usize) -> Result<String, ToolError> {
    for (i, op) in edits.iter().enumerate() {
        let edit_no = i + 1;
        let count = text.matches(op.old.as_str()).count();
        text = match count {
            0 => {
                // ws-tolerant fallback (unique + re-indented); then a Read-gutter
                // retry; else a recoverable no_match with the near-miss line.
                if let Some(s) = editmatch::ws_unique_replace(&text, &op.old, &op.new) {
                    s
                } else if let Some(stripped) = editmatch::strip_read_gutter(&op.old) {
                    match text.matches(&stripped).count() {
                        1 => text.replacen(&stripped, &op.new, 1),
                        _ => editmatch::ws_unique_replace(&text, &stripped, &op.new)
                            .ok_or_else(|| no_match(&text, &op.old, edit_no, total))?,
                    }
                } else {
                    return Err(no_match(&text, &op.old, edit_no, total));
                }
            }
            1 => text.replacen(op.old.as_str(), op.new.as_str(), 1),
            _n if op.replace_all => text.replace(op.old.as_str(), op.new.as_str()),
            n => {
                let lines = editmatch::exact_match_lines(&text, &op.old);
                let at = if lines.is_empty() {
                    String::new()
                } else {
                    format!(" at lines {lines:?}")
                };
                return Err(ToolError::new(
                    ErrClass::Edit,
                    "ambiguous",
                    format!("edit {edit_no} of {total}: '{}' appears {n} times{at}; pass replace_all=true", op.old),
                )
                .recoverable(true));
            }
        };
    }
    Ok(text)
}

fn no_match(text: &str, old: &str, edit_no: usize, total: usize) -> ToolError {
    let msg = editmatch::closest_ws_line(text, old).map_or_else(
        || format!("edit {edit_no} of {total}: '{old}' not found"),
        |line| format!("edit {edit_no} of {total}: '{old}' not found (closest match: line {line} differs only in whitespace)"),
    );
    ToolError::new(ErrClass::Edit, "no_match", msg).recoverable(true)
}

fn atomic_write(path: &str, bytes: &[u8]) -> Result<(), ToolError> {
    use std::io::Write;
    let p = std::path::Path::new(path);
    let pid = std::process::id();
    let tmp = p.with_extension(format!("tmp{pid}"));
    {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))?;
        f.write_all(bytes)
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))?;
        f.sync_all()
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))?;
    }
    std::fs::rename(&tmp, p).map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))
}

crate::origin_tool! {
    name: "MultiEdit",
    description: "Apply a sequence of edit operations to one file atomically (single read + single write). Each `old` must match the file's existing text verbatim — do NOT include Read's line-number/tab prefix; indentation/CRLF drift is tolerated.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Medium,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "file_path": { "type": "string" },
            "edits": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "properties": {
                        "old":         { "type": "string" },
                        "new":         { "type": "string" },
                        "replace_all": { "type": "boolean", "default": false }
                    },
                    "required": ["old", "new"]
                }
            }
        },
        "required": ["file_path", "edits"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::WriteCwd,
    token_budget: 4_000,
}
