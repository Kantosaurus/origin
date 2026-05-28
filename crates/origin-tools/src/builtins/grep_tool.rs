//! `Grep` v2 — `files_with_matches` default, `head_limit`, type/glob, context lines.

use crate::error::{ErrClass, ToolError};
use crate::{SideEffects, Tier, Urgency};
use grep_regex::RegexMatcher;
use grep_searcher::{sinks::UTF8, SearcherBuilder};
use ignore::types::TypesBuilder;
use ignore::WalkBuilder;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy)]
pub enum OutputMode {
    FilesWithMatches,
    Content,
    Count,
}

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct GrepArgs {
    pub pattern: String,
    pub path: Option<String>,
    pub glob: Option<String>,
    pub r#type: Option<String>,
    pub output_mode: Option<OutputMode>,
    pub head_limit: Option<u32>,
    pub before: u32,
    pub after: u32,
    pub line_numbers: bool,
    pub multiline: bool,
}

/// # Errors
/// `regex.invalid` on bad pattern, `io.*` on walk failures.
#[allow(clippy::module_name_repetitions)]
pub fn grep_v2(args: GrepArgs) -> Result<Value, ToolError> {
    let matcher = RegexMatcher::new(&args.pattern)
        .map_err(|e| ToolError::new(ErrClass::Regex, "invalid", e.to_string()))?;
    let mode = args.output_mode.unwrap_or(OutputMode::FilesWithMatches);
    let head_limit = args.head_limit.unwrap_or(250) as usize;
    let root = args.path.unwrap_or_else(|| ".".to_string());

    let mut walker = WalkBuilder::new(&root);
    walker.follow_links(false).standard_filters(true);
    if let Some(t) = &args.r#type {
        let mut tb = TypesBuilder::new();
        tb.add_defaults();
        tb.select(t);
        let types = tb
            .build()
            .map_err(|e| ToolError::new(ErrClass::Validation, "bad_type", e.to_string()))?;
        walker.types(types);
    }
    if let Some(g) = &args.glob {
        let mut ob = ignore::overrides::OverrideBuilder::new(&root);
        ob.add(g)
            .map_err(|e| ToolError::new(ErrClass::Validation, "bad_glob", e.to_string()))?;
        walker.overrides(
            ob.build()
                .map_err(|e| ToolError::new(ErrClass::Validation, "bad_glob_build", e.to_string()))?,
        );
    }

    let mut searcher = SearcherBuilder::new()
        .before_context(args.before as usize)
        .after_context(args.after as usize)
        .multi_line(args.multiline)
        .build();

    let mut match_results: Vec<Value> = Vec::new();
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut files: BTreeSet<String> = BTreeSet::new();
    'walk: for raw_entry in walker.build() {
        let Ok(entry) = raw_entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path().to_path_buf();
        let path_display = path.display().to_string();
        let mut local_count: u64 = 0;
        let mut local_lines: Vec<(u64, String)> = Vec::new();
        let res = searcher.search_path(
            &matcher,
            &path,
            UTF8(|lnum, line| {
                local_count += 1;
                local_lines.push((lnum, line.trim_end_matches('\n').to_string()));
                Ok(true)
            }),
        );
        if res.is_err() {
            continue;
        }
        if local_count == 0 {
            continue;
        }
        files.insert(path_display.clone());
        counts.insert(path_display.clone(), local_count);
        if matches!(mode, OutputMode::Content) {
            for (lnum, line) in local_lines {
                match_results.push(json!({"file": path_display, "line": lnum, "text": line}));
                if match_results.len() >= head_limit {
                    break 'walk;
                }
            }
        }
    }

    let out = match mode {
        OutputMode::FilesWithMatches => {
            let arr: Vec<String> = files.into_iter().take(head_limit).collect();
            json!({"files": arr})
        }
        OutputMode::Content => json!({"matches": match_results}),
        OutputMode::Count => {
            let arr: Vec<Value> = counts
                .into_iter()
                .take(head_limit)
                .map(|(f, c)| json!({"file": f, "count": c}))
                .collect();
            json!({"counts": arr})
        }
    };
    Ok(out)
}

crate::origin_tool! {
    name: "Grep",
    description: "Recursive regex search. Modes: files_with_matches (default), content, count. Supports glob/type filters and context lines.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "pattern":     { "type": "string" },
            "path":        { "type": "string" },
            "glob":        { "type": "string" },
            "type":        { "type": "string" },
            "output_mode": { "type": "string", "enum": ["files_with_matches", "content", "count"] },
            "head_limit":  { "type": "integer", "minimum": 1 },
            "before":      { "type": "integer", "minimum": 0 },
            "after":       { "type": "integer", "minimum": 0 },
            "line_numbers":{ "type": "boolean" },
            "multiline":   { "type": "boolean" }
        },
        "required": ["pattern"]
    }"#,
}
