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
    let mut text = text_fmt::normalise_to_lf(&bytes, &det)?;
    let mut applied = 0u32;
    let total = args.edits.len();
    for op in &args.edits {
        // `applied` is 0-based internally; report it 1-based to the model.
        let edit_no = applied + 1;
        // An empty needle matches between every character; replacing it would
        // splice `new` at every position and corrupt the file.
        if op.old.is_empty() {
            return Err(ToolError::new(
                ErrClass::Validation,
                "empty_old_string",
                format!("edit {edit_no} of {total}: old must not be empty"),
            )
            .recoverable(true));
        }
        let count = text.matches(op.old.as_str()).count();
        text = match count {
            0 => {
                // Whitespace-tolerant fallback (unique run only); else a
                // recoverable no_match with the closest near-miss line.
                if let Some(range) = editmatch::ws_tolerant_unique_range(&text, &op.old) {
                    let mut s = text.clone();
                    s.replace_range(range, &op.new);
                    s
                } else {
                    let msg = editmatch::closest_ws_line(&text, &op.old).map_or_else(
                        || format!("edit {edit_no} of {total}: '{}' not found", op.old),
                        |line| {
                            format!(
                                "edit {edit_no} of {total}: '{}' not found (closest match: line {line} differs only in whitespace)",
                                op.old
                            )
                        },
                    );
                    return Err(ToolError::new(ErrClass::Edit, "no_match", msg).recoverable(true));
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
        applied += 1;
    }
    let new_bytes = text_fmt::denormalise(&text, &det);
    atomic_write(&args.file_path, &new_bytes)?;
    Ok(json!({"ok": true, "applied": applied}))
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
