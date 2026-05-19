//! `Glob` tool — return paths matching a glob pattern.

use crate::{SideEffects, Tier, Urgency};

/// Resolve a glob pattern to a list of file paths (as strings).
///
/// # Errors
/// Returns a `String` describing a malformed pattern or filesystem walk failure.
#[allow(clippy::module_name_repetitions)] // `glob_tool` in module `glob_tool` — name kept for API clarity
pub fn glob_tool(pattern: &str) -> Result<Vec<String>, String> {
    let walker = glob::glob(pattern).map_err(|e| format!("bad pattern: {e}"))?;
    let mut out = Vec::new();
    for entry in walker {
        match entry {
            Ok(p) => {
                if let Some(s) = p.to_str() {
                    out.push(s.to_string());
                }
            }
            Err(e) => return Err(format!("walk error: {e}")),
        }
    }
    Ok(out)
}

crate::origin_tool! {
    name: "Glob",
    description: "List files matching a glob pattern. Supports ** for recursive descent.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "pattern": { "type": "string", "description": "Glob pattern; use ** for recursion" }
        },
        "required": ["pattern"]
    }"#,
}
