//! `Write` tool — create or overwrite a UTF-8 file with the given content.
//!
//! Unlike [`crate::builtins::edit::edit_tool`], `Write` does not require the
//! file to exist first — it creates missing parent directories so a single
//! tool call can land a brand-new file in a brand-new directory. This is the
//! workaround for the file-creation gap that previously forced models to
//! cobble together `Bash` + here-strings, which broke on Windows quoting.

use crate::{SideEffects, Tier, Urgency};

/// Write `content` to the file at `path`, creating any missing parent
/// directories. Overwrites the file if it already exists.
///
/// # Errors
/// Returns a `String` describing parent-directory or write failure.
#[allow(clippy::module_name_repetitions)] // `write_tool` in module `write` — name kept for API clarity
pub fn write_tool(path: &str, content: &str) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
    }
    std::fs::write(path, content).map_err(|e| format!("write: {e}"))
}

crate::origin_tool! {
    name: "Write",
    description: "Create or overwrite a UTF-8 file with the given content. Parent directories are created if missing.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Medium,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "path":    { "type": "string", "description": "File path (absolute or relative to cwd)" },
            "content": { "type": "string", "description": "Full file contents to write" }
        },
        "required": ["path", "content"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::WriteCwd,
}
