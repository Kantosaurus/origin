// SPDX-License-Identifier: Apache-2.0
//! `Edit` v2 — find-and-replace with CRLF safety, hunk return, `replace_all`.

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
pub fn edit_v2(args: EditArgs) -> Result<Value, ToolError> {
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
    let bytes = std::fs::read(&args.file_path)
        .map_err(|e| ToolError::new(ErrClass::Io, "not_found", format!("{}: {e}", args.file_path)))?;
    let det = text_fmt::detect(&bytes);
    let text = text_fmt::normalise_to_lf(&bytes, &det)?;

    let count = text.matches(&args.old_string).count();
    let updated = match count {
        0 => {
            return Err(ToolError::new(
                ErrClass::Edit,
                "no_match",
                format!("'{}' not found in {}", args.old_string, args.file_path),
            )
            .recoverable(true)
            .hint("widen the needle or add surrounding context"));
        }
        1 => text.replacen(&args.old_string, &args.new_string, 1),
        _ if args.replace_all => text.replace(&args.old_string, &args.new_string),
        n => {
            return Err(ToolError::new(
                ErrClass::Edit,
                "ambiguous",
                format!(
                    "'{}' appears {n} times; pass replace_all=true or widen the needle",
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
    description: "Find-and-replace a unique string in a file. CRLF-safe. Pass replace_all=true for multi-match.",
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
