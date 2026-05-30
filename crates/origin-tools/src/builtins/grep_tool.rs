// SPDX-License-Identifier: Apache-2.0
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

    // Code-structure-aware grep (jcode agentgrep): when `ORIGIN_AGENTGREP=1`,
    // each `content`-mode match is annotated with the nearest enclosing
    // definition (`fn`/`def`/`class`/…) so the model gets structure, not just a
    // raw line. Default-off ⇒ the result shape is byte-identical to before.
    let agentgrep = std::env::var("ORIGIN_AGENTGREP").as_deref() == Ok("1");

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
            // Read the whole file once (only when agentgrep is on) so each
            // match can resolve its enclosing definition by a cheap backward scan.
            let file_lines: Vec<String> = if agentgrep {
                std::fs::read_to_string(&path)
                    .map(|s| s.lines().map(str::to_string).collect())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            for (lnum, line) in local_lines {
                let mut obj = json!({"file": path_display, "line": lnum, "text": line});
                if agentgrep {
                    if let Some(sym) = enclosing_symbol(&file_lines, lnum) {
                        if let Some(map) = obj.as_object_mut() {
                            map.insert("symbol".to_string(), Value::String(sym));
                        }
                    }
                }
                match_results.push(obj);
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

/// Definition leaders recognised by the agentgrep enclosing-symbol scan. Kept
/// deliberately language-agnostic (Rust / Python / TS-JS / Go / C-family) so it
/// works without a per-language parser; this is a heuristic, not tree-sitter.
const DEF_LEADERS: &[&str] = &[
    "fn ", "pub fn ", "async fn ", "pub async fn ", "def ", "async def ", "class ", "struct ",
    "enum ", "trait ", "impl ", "interface ", "func ", "function ", "module ", "namespace ",
    "type ", "const fn ", "pub const fn ", "public ", "private ", "protected ", "export function ",
    "export default function ", "export class ", "export const ",
];

/// Resolve the nearest enclosing definition for a 1-based match line by scanning
/// backwards for the first line whose trimmed text starts with a known
/// definition leader. Returns the trimmed definition line (bounded length), or
/// `None` when no definition precedes the match.
fn enclosing_symbol(lines: &[String], match_line_1based: u64) -> Option<String> {
    if lines.is_empty() || match_line_1based == 0 {
        return None;
    }
    // Convert to a 0-based index, clamped to the file length.
    let match_idx = usize::try_from(match_line_1based).unwrap_or(usize::MAX);
    let start = match_idx.min(lines.len()).saturating_sub(1);
    for idx in (0..=start).rev() {
        let trimmed = lines[idx].trim_start();
        if DEF_LEADERS.iter().any(|lead| trimmed.starts_with(lead)) {
            let sym: String = trimmed.trim_end().chars().take(120).collect();
            return Some(sym);
        }
    }
    None
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

#[cfg(test)]
mod agentgrep_tests {
    use super::enclosing_symbol;

    fn lines(src: &str) -> Vec<String> {
        src.lines().map(str::to_string).collect()
    }

    #[test]
    fn finds_nearest_preceding_rust_fn() {
        let src = "mod a {\n    pub fn target() {\n        let x = 1;\n        do_thing(x);\n    }\n}\n";
        // The match for `do_thing` is on line 4 (1-based).
        let sym = enclosing_symbol(&lines(src), 4).expect("symbol");
        assert_eq!(sym, "pub fn target() {");
    }

    #[test]
    fn finds_python_def() {
        let src = "class C:\n    def method(self):\n        return compute()\n";
        let sym = enclosing_symbol(&lines(src), 3).expect("symbol");
        assert_eq!(sym, "def method(self):");
    }

    #[test]
    fn none_when_no_definition_precedes() {
        let src = "let x = 1;\nlet y = 2;\n";
        assert!(enclosing_symbol(&lines(src), 2).is_none());
    }

    #[test]
    fn out_of_range_line_is_safe() {
        let src = "fn a() {}\n";
        // Asking past the end of the file must not panic.
        assert!(enclosing_symbol(&lines(src), 999).is_some());
        assert!(enclosing_symbol(&[], 1).is_none());
        assert!(enclosing_symbol(&lines(src), 0).is_none());
    }
}
