//! `Read` tool — reads a UTF-8 text file in full.

use crate::{SideEffects, Tier, Urgency};

/// Read the contents of a UTF-8 text file.
///
/// # Errors
/// Returns `io::Error` if the file cannot be opened or is not valid UTF-8.
#[allow(clippy::module_name_repetitions)] // `read_tool` in module `read` — name kept for API clarity
pub fn read_tool(path: &str) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

crate::origin_tool! {
    name: "Read",
    description: "Read the contents of a UTF-8 text file at the given path.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Absolute file path" }
        },
        "required": ["path"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::ReadFs,
}
