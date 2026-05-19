//! `Grep` tool — search files under a root for a regex pattern.
//!
//! Returns lines as `"<path>:<line_number>: <line_content>"`.

use crate::{SideEffects, Tier, Urgency};
use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::Searcher;
use walkdir::WalkDir;

/// Search for `pattern` (regex) under `root` (directory or file).
///
/// # Errors
/// Returns a `String` describing a bad regex, walk failure, or read failure.
#[allow(clippy::module_name_repetitions)] // function shares module name; required by registry naming
pub fn grep_tool(pattern: &str, root: &str) -> Result<Vec<String>, String> {
    let matcher = RegexMatcher::new(pattern).map_err(|e| format!("regex error: {e}"))?;
    let mut results = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|e| format!("walk error: {e}"))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_path_buf();
        let path_display = path.display().to_string();
        let mut searcher = Searcher::new();
        let mut local: Vec<String> = Vec::new();
        let sink_path = path_display.clone();
        let res = searcher.search_path(
            &matcher,
            &path,
            UTF8(|lnum, line| {
                local.push(format!("{sink_path}:{lnum}: {}", line.trim_end_matches('\n')));
                Ok(true)
            }),
        );
        if let Err(e) = res {
            // Skip non-utf8 / binary files (these aren't grep targets).
            let s = e.to_string();
            if !s.contains("invalid utf-8") {
                return Err(format!("read error in {path_display}: {e}"));
            }
        }
        results.extend(local);
    }
    Ok(results)
}

crate::origin_tool! {
    name: "Grep",
    description: "Recursively search files under a root for a regex pattern. Returns path:line:content.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "pattern": { "type": "string", "description": "Regex pattern" },
            "root":    { "type": "string", "description": "Directory or file path" }
        },
        "required": ["pattern", "root"]
    }"#,
}
