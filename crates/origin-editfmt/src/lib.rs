// SPDX-License-Identifier: Apache-2.0
//! Model-tuned edit-format matrix for applying LLM-produced edits.
//!
//! This crate parses several common edit formats into a normalized
//! [`Hunk`] representation and applies them against original file
//! contents. It also exposes a per-model best-format table so callers
//! can pick the format a given model is most reliable with.

#![forbid(unsafe_code)]

use std::fmt;

/// The edit format used to encode a model's proposed change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EditFormat {
    /// `aider`-style SEARCH/REPLACE blocks with `<<<<<<<`/`=======`/`>>>>>>>` markers.
    SearchReplace,
    /// A fenced diff block (```diff ... ```) wrapping SEARCH/REPLACE content.
    DiffFenced,
    /// A full replacement of the file's contents.
    WholeFile,
    /// A minimal unified diff (`--- `/`+++ `/`@@` hunks).
    Udiff,
}

impl fmt::Display for EditFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::SearchReplace => "search-replace",
            Self::DiffFenced => "diff-fenced",
            Self::WholeFile => "whole-file",
            Self::Udiff => "udiff",
        };
        f.write_str(s)
    }
}

/// A normalized edit: replace `before` with `after` inside `file`.
///
/// For [`EditFormat::WholeFile`], `before` is empty and `after` holds
/// the complete new file contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// Target file path as named by the model.
    pub file: String,
    /// The text to locate in the original (empty for whole-file edits).
    pub before: String,
    /// The replacement text.
    pub after: String,
}

/// Errors produced while parsing or applying an edit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditFmtError {
    /// The input did not match the expected structure for the format.
    #[error("parse error: {0}")]
    Parse(String),
    /// The `before` text was not found in the original contents.
    #[error("no match: {0}")]
    NoMatch(String),
    /// The `before` text matched more than once, so the edit is ambiguous.
    #[error("ambiguous match: {0}")]
    Ambiguous(String),
}

/// Parses `text` in the given `format` into a list of normalized hunks.
///
/// # Errors
///
/// Returns [`EditFmtError::Parse`] when the input does not contain a
/// well-formed block for the requested format.
pub fn parse(format: EditFormat, text: &str) -> Result<Vec<Hunk>, EditFmtError> {
    match format {
        EditFormat::SearchReplace => parse_search_replace(text, false),
        EditFormat::DiffFenced => parse_search_replace(text, true),
        EditFormat::WholeFile => parse_whole_file(text),
        EditFormat::Udiff => parse_udiff(text),
    }
}

/// Applies a single hunk to `original`, returning the new contents.
///
/// For [`Hunk`]s whose `before` is empty (whole-file edits) the original
/// is replaced wholesale by `after`. Otherwise the unique occurrence of
/// `before` is replaced by `after`.
///
/// # Errors
///
/// Returns [`EditFmtError::NoMatch`] when `before` is absent and
/// [`EditFmtError::Ambiguous`] when `before` occurs more than once.
pub fn apply(hunk: &Hunk, original: &str) -> Result<String, EditFmtError> {
    if hunk.before.is_empty() {
        return Ok(hunk.after.clone());
    }
    let first = original.find(&hunk.before);
    let Some(idx) = first else {
        return Err(EditFmtError::NoMatch(hunk.file.clone()));
    };
    let next = original[idx + hunk.before.len()..].find(&hunk.before);
    if next.is_some() {
        return Err(EditFmtError::Ambiguous(hunk.file.clone()));
    }
    let mut out = String::with_capacity(original.len() - hunk.before.len() + hunk.after.len());
    out.push_str(&original[..idx]);
    out.push_str(&hunk.after);
    out.push_str(&original[idx + hunk.before.len()..]);
    Ok(out)
}

/// Returns the edit format that the named `model` is most reliable with.
///
/// The match is case-insensitive and prefix-based. Unknown models fall
/// back to [`EditFormat::SearchReplace`].
#[must_use]
pub fn best_format_for(model: &str) -> EditFormat {
    let m = model.to_ascii_lowercase();
    if m.starts_with("claude") || m.contains("anthropic") || m.starts_with("sonnet")
        || m.starts_with("opus") || m.starts_with("haiku")
    {
        return EditFormat::SearchReplace;
    }
    if m.starts_with("gpt-4") || m.starts_with("gpt4") || m.starts_with("o1") || m.starts_with("o3")
    {
        return EditFormat::Udiff;
    }
    if m.starts_with("deepseek") {
        return EditFormat::DiffFenced;
    }
    if m.starts_with("gpt-3.5") || m.contains("turbo-instruct") {
        return EditFormat::WholeFile;
    }
    EditFormat::SearchReplace
}

/// Optional filename hint preceding a search/replace block.
fn extract_filename(lines: &[&str], block_start: usize) -> String {
    // Walk backwards over blank lines / fence lines to find a path-ish line.
    let mut i = block_start;
    while i > 0 {
        i -= 1;
        let trimmed = lines[i].trim();
        if trimmed.is_empty() || trimmed.starts_with("```") {
            continue;
        }
        return trimmed.trim_end_matches(':').to_string();
    }
    String::new()
}

/// Shared parser for SEARCH/REPLACE blocks (optionally inside a diff fence).
fn parse_search_replace(text: &str, require_fence: bool) -> Result<Vec<Hunk>, EditFmtError> {
    if require_fence && !text.contains("```") {
        return Err(EditFmtError::Parse("missing diff fence".to_string()));
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut hunks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_start().starts_with("<<<<<<<") {
            let file = extract_filename(&lines, i);
            let mut before = String::new();
            let mut after = String::new();
            let mut j = i + 1;
            let mut seen_divider = false;
            let mut closed = false;
            while j < lines.len() {
                let line = lines[j];
                if line.trim_start().starts_with("=======") {
                    seen_divider = true;
                    j += 1;
                    continue;
                }
                if line.trim_start().starts_with(">>>>>>>") {
                    closed = true;
                    break;
                }
                if seen_divider {
                    after.push_str(line);
                    after.push('\n');
                } else {
                    before.push_str(line);
                    before.push('\n');
                }
                j += 1;
            }
            if !seen_divider || !closed {
                return Err(EditFmtError::Parse(
                    "unterminated search/replace block".to_string(),
                ));
            }
            hunks.push(Hunk {
                file,
                before: trim_trailing_newline(before),
                after: trim_trailing_newline(after),
            });
            i = j + 1;
        } else {
            i += 1;
        }
    }
    if hunks.is_empty() {
        return Err(EditFmtError::Parse(
            "no search/replace block found".to_string(),
        ));
    }
    Ok(hunks)
}

/// Removes exactly one trailing `\n` accumulated during line collection.
fn trim_trailing_newline(mut s: String) -> String {
    if s.ends_with('\n') {
        s.pop();
    }
    s
}

/// Parses a whole-file block: an optional `file:` header then fenced contents,
/// or the raw text if no fence is present.
fn parse_whole_file(text: &str) -> Result<Vec<Hunk>, EditFmtError> {
    let lines: Vec<&str> = text.lines().collect();
    let mut file = String::new();
    let mut content_lines: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let mut saw_fence = false;
    for line in &lines {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if in_fence {
                in_fence = false;
            } else {
                in_fence = true;
                saw_fence = true;
            }
            continue;
        }
        if in_fence {
            content_lines.push(line);
        } else if file.is_empty() && !trimmed.is_empty() && !saw_fence {
            file = trimmed.trim_end_matches(':').to_string();
        }
    }
    let content = if saw_fence {
        content_lines.join("\n")
    } else {
        // No fence: everything after an optional first header line is the body.
        text.to_string()
    };
    if content.is_empty() && file.is_empty() {
        return Err(EditFmtError::Parse("empty whole-file block".to_string()));
    }
    Ok(vec![Hunk {
        file,
        before: String::new(),
        after: content,
    }])
}

/// Parses a minimal unified diff into normalized hunks (one per `@@` section).
fn parse_udiff(text: &str) -> Result<Vec<Hunk>, EditFmtError> {
    let lines: Vec<&str> = text.lines().collect();
    let mut file = String::new();
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut before = String::new();
    let mut after = String::new();
    let mut in_hunk = false;
    let mut any_hunk = false;

    let flush = |before: &mut String, after: &mut String, file: &str, hunks: &mut Vec<Hunk>| {
        hunks.push(Hunk {
            file: file.to_string(),
            before: trim_trailing_newline(std::mem::take(before)),
            after: trim_trailing_newline(std::mem::take(after)),
        });
    };

    for line in &lines {
        if let Some(rest) = line.strip_prefix("--- ") {
            file = clean_diff_path(rest);
            continue;
        }
        if let Some(rest) = line.strip_prefix("+++ ") {
            // Prefer the new-file path when present and meaningful.
            let p = clean_diff_path(rest);
            if !p.is_empty() {
                file = p;
            }
            continue;
        }
        if line.starts_with("@@") {
            if in_hunk {
                flush(&mut before, &mut after, &file, &mut hunks);
            }
            in_hunk = true;
            any_hunk = true;
            continue;
        }
        if !in_hunk {
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            after.push_str(rest);
            after.push('\n');
        } else if let Some(rest) = line.strip_prefix('-') {
            before.push_str(rest);
            before.push('\n');
        } else {
            let ctx = line.strip_prefix(' ').unwrap_or(line);
            before.push_str(ctx);
            before.push('\n');
            after.push_str(ctx);
            after.push('\n');
        }
    }
    if !any_hunk {
        return Err(EditFmtError::Parse("no @@ hunk header found".to_string()));
    }
    if in_hunk {
        flush(&mut before, &mut after, &file, &mut hunks);
    }
    Ok(hunks)
}

/// Strips a `a/`/`b/` prefix and trailing timestamp from a diff path.
fn clean_diff_path(raw: &str) -> String {
    let path = raw.split('\t').next().unwrap_or(raw).trim();
    if path == "/dev/null" {
        return String::new();
    }
    let stripped = path
        .strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path);
    stripped.to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn parses_search_replace_block() {
        let text = "src/main.rs\n\
                    <<<<<<< SEARCH\n\
                    let x = 1;\n\
                    =======\n\
                    let x = 2;\n\
                    >>>>>>> REPLACE\n";
        let hunks = parse(EditFormat::SearchReplace, text).unwrap();
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].file, "src/main.rs");
        assert_eq!(hunks[0].before, "let x = 1;");
        assert_eq!(hunks[0].after, "let x = 2;");
    }

    #[test]
    fn apply_replaces_exactly_once() {
        let hunk = Hunk {
            file: "f".to_string(),
            before: "foo".to_string(),
            after: "bar".to_string(),
        };
        let out = apply(&hunk, "a foo b").unwrap();
        assert_eq!(out, "a bar b");
    }

    #[test]
    fn apply_no_match_errors() {
        let hunk = Hunk {
            file: "f".to_string(),
            before: "zzz".to_string(),
            after: "bar".to_string(),
        };
        let err = apply(&hunk, "a foo b").unwrap_err();
        assert!(matches!(err, EditFmtError::NoMatch(_)));
    }

    #[test]
    fn apply_ambiguous_errors() {
        let hunk = Hunk {
            file: "f".to_string(),
            before: "foo".to_string(),
            after: "bar".to_string(),
        };
        let err = apply(&hunk, "foo and foo").unwrap_err();
        assert!(matches!(err, EditFmtError::Ambiguous(_)));
    }

    #[test]
    fn apply_whole_file_replaces() {
        let hunk = Hunk {
            file: "f".to_string(),
            before: String::new(),
            after: "brand new".to_string(),
        };
        let out = apply(&hunk, "anything at all").unwrap();
        assert_eq!(out, "brand new");
    }

    #[test]
    fn parses_whole_file_fenced() {
        let text = "config.toml\n```\nkey = \"value\"\nother = 1\n```\n";
        let hunks = parse(EditFormat::WholeFile, text).unwrap();
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].file, "config.toml");
        assert!(hunks[0].before.is_empty());
        assert_eq!(hunks[0].after, "key = \"value\"\nother = 1");
    }

    #[test]
    fn parses_small_udiff() {
        let text = "--- a/src/lib.rs\n\
                    +++ b/src/lib.rs\n\
                    @@ -1,3 +1,3 @@\n\
                    ctx line\n\
                    -old line\n\
                    +new line\n\
                    more ctx\n";
        let hunks = parse(EditFormat::Udiff, text).unwrap();
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].file, "src/lib.rs");
        assert_eq!(hunks[0].before, "ctx line\nold line\nmore ctx");
        assert_eq!(hunks[0].after, "ctx line\nnew line\nmore ctx");
    }

    #[test]
    fn udiff_round_trips_through_apply() {
        let original = "ctx line\nold line\nmore ctx";
        let text = "--- a/x\n+++ b/x\n@@ -1,3 +1,3 @@\n ctx line\n-old line\n+new line\n more ctx\n";
        let hunks = parse(EditFormat::Udiff, text).unwrap();
        let out = apply(&hunks[0], original).unwrap();
        assert_eq!(out, "ctx line\nnew line\nmore ctx");
    }

    #[test]
    fn diff_fenced_requires_fence() {
        let no_fence = "<<<<<<< SEARCH\na\n=======\nb\n>>>>>>> REPLACE\n";
        assert!(matches!(
            parse(EditFormat::DiffFenced, no_fence),
            Err(EditFmtError::Parse(_))
        ));
        let fenced = "file.py\n```diff\n<<<<<<< SEARCH\na\n=======\nb\n>>>>>>> REPLACE\n```\n";
        let hunks = parse(EditFormat::DiffFenced, fenced).unwrap();
        assert_eq!(hunks[0].before, "a");
        assert_eq!(hunks[0].after, "b");
    }

    #[test]
    fn best_format_for_known_models() {
        assert_eq!(best_format_for("claude-3-5-sonnet"), EditFormat::SearchReplace);
        assert_eq!(best_format_for("Opus-4"), EditFormat::SearchReplace);
        assert_eq!(best_format_for("gpt-4o"), EditFormat::Udiff);
        assert_eq!(best_format_for("o3-mini"), EditFormat::Udiff);
        assert_eq!(best_format_for("deepseek-coder"), EditFormat::DiffFenced);
        assert_eq!(best_format_for("gpt-3.5-turbo"), EditFormat::WholeFile);
    }

    #[test]
    fn best_format_for_unknown_defaults() {
        assert_eq!(best_format_for("some-random-model"), EditFormat::SearchReplace);
        assert_eq!(best_format_for(""), EditFormat::SearchReplace);
    }

    #[test]
    fn parse_missing_block_errors() {
        let err = parse(EditFormat::SearchReplace, "just some prose").unwrap_err();
        assert!(matches!(err, EditFmtError::Parse(_)));
    }

    #[test]
    fn display_renders_format_names() {
        assert_eq!(EditFormat::SearchReplace.to_string(), "search-replace");
        assert_eq!(EditFormat::Udiff.to_string(), "udiff");
    }
}
