//! `MultiEdit` — apply a list of edit operations to one file, atomically.

use crate::error::{ErrClass, ToolError};
use crate::text_fmt;
use crate::{SideEffects, Tier, Urgency};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct EditOp { pub old: String, pub new: String, pub replace_all: bool }

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct MultiEditArgs {
    pub file_path: String,
    pub edits: Vec<EditOp>,
}

/// # Errors
/// `edit.no_match | edit.ambiguous | io.*`
pub fn multi_edit(args: &MultiEditArgs) -> Result<Value, ToolError> {
    let bytes = std::fs::read(&args.file_path).map_err(|e| {
        ToolError::new(ErrClass::Io, "not_found", format!("{}: {e}", args.file_path))
    })?;
    let det = text_fmt::detect(&bytes);
    let mut text = text_fmt::normalise_to_lf(&bytes, &det)?;
    let mut applied = 0u32;
    for op in &args.edits {
        let count = text.matches(op.old.as_str()).count();
        text = match count {
            0 => return Err(ToolError::new(ErrClass::Edit, "no_match",
                format!("edit {applied} of {}: '{}' not found", args.edits.len(), op.old))),
            1 => text.replacen(op.old.as_str(), op.new.as_str(), 1),
            _n if op.replace_all => text.replace(op.old.as_str(), op.new.as_str()),
            n => return Err(ToolError::new(ErrClass::Edit, "ambiguous",
                format!("edit {applied} of {}: '{}' appears {n} times; pass replace_all=true", args.edits.len(), op.old))),
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
        f.write_all(bytes).map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))?;
        f.sync_all().map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))?;
    }
    std::fs::rename(&tmp, p).map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))
}

crate::origin_tool! {
    name: "MultiEdit",
    description: "Apply a sequence of edit operations to one file atomically. Single read + single write per call.",
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
