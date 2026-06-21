// SPDX-License-Identifier: Apache-2.0
//! `Edit` v2 — find-and-replace with CRLF safety, hunk return, `replace_all`.

use crate::builtins::editmatch;
use crate::builtins::write::WriteGuard;
use crate::error::{ErrClass, ToolError};
use crate::text_fmt;
use crate::{SideEffects, Tier, Urgency};
use serde_json::{json, Value};

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct EditArgs {
    pub file_path: String,
    pub old_string: String,
    pub new_string: String,
    pub replace_all: bool,
}

/// Find-and-replace `old_string` with `new_string` in `file_path`.
///
/// Operates on LF-normalised text; writes back in the file's original EOL
/// convention, preserving per-source-line EOL for mixed-EOL files.
///
/// # Errors
/// Returns `ToolError(edit.no_match | edit.ambiguous | io.*)`.
#[allow(clippy::module_name_repetitions)]
#[allow(clippy::needless_pass_by_value)]
pub fn edit_v2(args: EditArgs, guard: Option<&WriteGuard>) -> Result<Value, ToolError> {
    // An empty needle matches between every character; `str::replace("", ..)`
    // would splice `new_string` at every position, corrupting the file.
    if args.old_string.is_empty() {
        return Err(ToolError::new(
            ErrClass::Validation,
            "empty_old_string",
            "old_string must not be empty",
        )
        .recoverable(true)
        .hint("provide the exact text to replace"));
    }
    // A no-op edit (old == new) silently "succeeds" while changing nothing — a
    // wasted turn the model reads as progress. Reject it so it self-corrects.
    if args.old_string == args.new_string {
        return Err(ToolError::new(
            ErrClass::Edit,
            "noop",
            "new_string equals old_string; this edit changes nothing",
        )
        .recoverable(true)
        .hint("edit a different region, or omit this edit"));
    }
    // Read-before-edit guard (mirrors Write): refuse to edit a file the model has
    // not Read this session, so `old_string` reflects observed content rather than
    // a hallucinated mental model. `None` (tests/headless) ⇒ no guard.
    if let Some(g) = guard {
        if !g.has_read(&args.file_path) {
            return Err(ToolError::new(
                ErrClass::Edit,
                "read_required",
                format!("refusing to Edit '{}' that has not been Read this session", args.file_path),
            )
            .recoverable(true)
            .hint("call Read on this file first, then re-Edit"));
        }
    }
    let bytes = std::fs::read(&args.file_path)
        .map_err(|e| ToolError::new(ErrClass::Io, "not_found", format!("{}: {e}", args.file_path)))?;
    let det = text_fmt::detect(&bytes);
    let text = text_fmt::normalise_to_lf(&bytes, &det)?;

    let count = text.matches(&args.old_string).count();
    let updated = match count {
        0 => {
            // Exact match failed. Try ONE whitespace-tolerant fallback that
            // absorbs indentation / trailing-whitespace drift (re-indenting the
            // replacement to the matched region) — but only when it resolves to a
            // UNIQUE run (it never guesses). If that also fails and the needle
            // looks like a pasted Read line-number gutter, retry against the
            // gutter-stripped needle (the ws-fallback can't fix digit drift).
            // Otherwise return no_match enriched with the closest near-miss.
            if let Some(s) = editmatch::ws_unique_replace(&text, &args.old_string, &args.new_string) {
                s
            } else if let Some(stripped) = editmatch::strip_read_gutter(&args.old_string) {
                let updated = match text.matches(&stripped).count() {
                    1 => text.replacen(&stripped, &args.new_string, 1),
                    _ => editmatch::ws_unique_replace(&text, &stripped, &args.new_string)
                        .ok_or_else(|| gutter_or_no_match(&text, &args.old_string, &args.file_path))?,
                };
                updated
            } else {
                return Err(gutter_or_no_match(&text, &args.old_string, &args.file_path));
            }
        }
        1 => text.replacen(&args.old_string, &args.new_string, 1),
        _ if args.replace_all => text.replace(&args.old_string, &args.new_string),
        n => {
            let lines = editmatch::exact_match_lines(&text, &args.old_string);
            let at = if lines.is_empty() {
                String::new()
            } else {
                format!(" at lines {lines:?}")
            };
            return Err(ToolError::new(
                ErrClass::Edit,
                "ambiguous",
                format!(
                    "'{}' appears {n} times{at}; pass replace_all=true or add surrounding context",
                    args.old_string
                ),
            )
            .recoverable(true));
        }
    };

    let hunk = build_hunk(&text, &args.old_string, &args.new_string);
    let new_bytes = text_fmt::denormalise(&updated, &det);
    atomic_write(&args.file_path, &new_bytes)?;
    Ok(json!({
        "ok": true,
        "hunks": [hunk],
    }))
}

/// The `edit.no_match` error, enriched with the closest whitespace-only
/// near-miss line so the model can correct in one step.
fn gutter_or_no_match(text: &str, old: &str, path: &str) -> ToolError {
    let msg = editmatch::closest_ws_line(text, old).map_or_else(
        || format!("'{old}' not found in {path}"),
        |line| format!("'{old}' not found in {path} (closest match: line {line} differs only in whitespace)"),
    );
    ToolError::new(ErrClass::Edit, "no_match", msg)
        .recoverable(true)
        .hint("widen the needle or add surrounding context")
}

fn build_hunk(before: &str, old: &str, new: &str) -> Value {
    let line = before
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains(old))
        .map_or(0, |(i, _)| i + 1);
    json!({ "before": old, "after": new, "line": line })
}

fn atomic_write(path: &str, bytes: &[u8]) -> Result<(), ToolError> {
    use std::io::Write;
    let p = std::path::Path::new(path);
    let pid = std::process::id();
    let tmp = p.with_extension(format!("tmp{pid}"));
    {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", format!("create tmp: {e}")))?;
        f.write_all(bytes)
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", format!("write tmp: {e}")))?;
        f.sync_all()
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", format!("fsync: {e}")))?;
    }
    std::fs::rename(&tmp, p)
        .map_err(|e| ToolError::new(ErrClass::Io, "permission", format!("rename: {e}")))?;
    Ok(())
}

crate::origin_tool! {
    name: "Edit",
    description: "Find-and-replace a unique string in a file. `old_string` must match the file's existing text verbatim — do NOT include Read's line-number/tab prefix. Indentation/CRLF drift is tolerated; on no match the error names the closest near-miss line. Pass replace_all=true for multi-match.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Medium,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "file_path":   { "type": "string" },
            "old_string":  { "type": "string" },
            "new_string":  { "type": "string" },
            "replace_all": { "type": "boolean", "default": false }
        },
        "required": ["file_path", "old_string", "new_string"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::WriteCwd,
    token_budget: 4_000,
}
