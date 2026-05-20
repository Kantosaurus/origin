//! `Edit` tool — find-and-replace a unique string in a UTF-8 file.

use crate::{SideEffects, Tier, Urgency};

/// Replace `old` with `new` in the file at `path`. `old` must appear exactly once.
///
/// # Errors
/// Returns a `String` describing not-found, ambiguous (multiple matches), or I/O failure.
#[allow(clippy::module_name_repetitions)] // `edit_tool` in module `edit` — name kept for API clarity
pub fn edit_tool(path: &str, old: &str, new: &str) -> Result<(), String> {
    let contents = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let count = contents.matches(old).count();
    match count {
        0 => Err(format!("'{old}' not found in {path}")),
        1 => {
            let updated = contents.replacen(old, new, 1);
            std::fs::write(path, updated).map_err(|e| format!("write: {e}"))?;
            Ok(())
        }
        n => Err(format!(
            "'{old}' is not unique in {path} ({n} occurrences); refine the search string"
        )),
    }
}

crate::origin_tool! {
    name: "Edit",
    description: "Find-and-replace a unique string in a file. Errors if old_string is missing or ambiguous.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Medium,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "path":       { "type": "string" },
            "old_string": { "type": "string" },
            "new_string": { "type": "string" }
        },
        "required": ["path", "old_string", "new_string"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::WriteCwd,
}
