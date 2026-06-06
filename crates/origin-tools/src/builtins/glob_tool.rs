// SPDX-License-Identifier: Apache-2.0
//! `Glob` v2 — gitignore-aware, mtime-sorted, head-limited.

use crate::error::{ErrClass, ToolError};
use crate::{SideEffects, Tier, Urgency};
use ignore::WalkBuilder;
use serde_json::Value;
use std::time::SystemTime;

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct GlobArgs {
    pub pattern: String,
    pub path: Option<String>,
    pub head_limit: Option<u32>,
}

/// # Errors
/// `validation.bad_glob` on bad pattern, `io.*` on walk failures.
#[allow(clippy::module_name_repetitions)]
pub fn glob_v2(args: GlobArgs) -> Result<Value, ToolError> {
    let root = args.path.unwrap_or_else(|| ".".to_string());
    let head_limit = args.head_limit.unwrap_or(250) as usize;

    // Build a globset to match the pattern against each file path.
    // We walk the tree with standard_filters (honours gitignore) and
    // then post-filter against the glob pattern so that gitignore rules
    // are *not* bypassed by an override whitelist.
    let glob = globset::GlobBuilder::new(&args.pattern)
        .literal_separator(false)
        .build()
        .map_err(|e| ToolError::new(ErrClass::Validation, "bad_glob", e.to_string()))?;
    let glob_set = globset::GlobSetBuilder::new()
        .add(glob)
        .build()
        .map_err(|e| ToolError::new(ErrClass::Validation, "bad_glob_build", e.to_string()))?;

    let walker = WalkBuilder::new(&root)
        .follow_links(false)
        .standard_filters(true)
        .build();

    let mut file_entries: Vec<(String, SystemTime)> = Vec::new();
    for raw_entry in walker {
        let Ok(entry) = raw_entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        // Match relative path against the pattern so "*.rs" works correctly.
        let rel = entry.path().strip_prefix(&root).unwrap_or_else(|_| entry.path());
        if !glob_set.is_match(rel) {
            continue;
        }
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        file_entries.push((entry.path().display().to_string(), mtime));
    }
    file_entries.sort_by_key(|e| std::cmp::Reverse(e.1));
    let arr: Vec<Value> = file_entries
        .into_iter()
        .take(head_limit)
        .map(|(p, _)| Value::String(p))
        .collect();
    Ok(Value::Array(arr))
}

crate::origin_tool! {
    name: "Glob",
    description: "Find files matching a glob pattern. Returns paths sorted by mtime DESC, gitignore-aware.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "pattern":    { "type": "string" },
            "path":       { "type": "string" },
            "head_limit": { "type": "integer", "minimum": 1 }
        },
        "required": ["pattern"]
    }"#,
}
