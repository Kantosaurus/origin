// SPDX-License-Identifier: Apache-2.0
//! Editor-agnostic watcher for AI-trigger comments in source files.
//!
//! Scans source trees for inline markers such as `// AI: ...`, `# AI! ...`,
//! or `-- AI? ...` and reports them as actionable items, mirroring the
//! `aider --watch-files` workflow without depending on any editor.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use thiserror::Error;
use walkdir::WalkDir;

/// Errors returned while walking a directory tree.
#[derive(Debug, Error)]
pub enum WatchError {
    /// An I/O error occurred while reading the tree or a file.
    #[error("io error: {0}")]
    Io(String),
}

/// The flavor of an AI-trigger marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiKind {
    /// A plain `AI` marker (informational / general instruction).
    Ai,
    /// An `AI!` marker (act now / high priority).
    Bang,
    /// An `AI?` marker (a question for the assistant).
    Question,
}

/// A single AI-trigger comment located in a source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiComment {
    /// Path of the file the comment was found in.
    pub file: String,
    /// One-based line number of the comment.
    pub line: u32,
    /// The kind of marker that was detected.
    pub kind: AiKind,
    /// The instruction text following the marker.
    pub text: String,
}

/// Configuration for a directory scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanConfig {
    /// Root directory to walk.
    pub root: String,
    /// File extensions (without the leading dot) that should be scanned.
    pub extensions: Vec<String>,
}

/// Comment-leader tokens recognized across common languages.
///
/// Ordered longest-first so that, e.g., `--` is preferred over a single `-`.
const LEADERS: [&str; 5] = ["//", "--", "#", ";", "%"];

/// Detects a trailing AI-trigger marker in a single line of source.
///
/// Recognizes a comment leader (`//`, `#`, `--`, `;`, `%`) followed by the
/// token `AI`, `AI!`, or `AI?`, optionally separated from the instruction by
/// a colon, dash, or whitespace. Returns the detected [`AiKind`] together with
/// the trimmed instruction text. Returns [`None`] when no marker is present.
///
/// The detection is pure and performs no I/O.
#[must_use]
pub fn parse_line(line: &str) -> Option<(AiKind, String)> {
    // Find the first comment leader and parse what follows it. Try every
    // leader occurrence so a marker later on the line is still detected.
    for leader in LEADERS {
        let mut search_from = 0usize;
        while let Some(rel) = line[search_from..].find(leader) {
            let after = search_from + rel + leader.len();
            if let Some(found) = parse_after_leader(&line[after..]) {
                return Some(found);
            }
            search_from = after;
        }
    }
    None
}

/// Parses the marker token immediately following a comment leader.
fn parse_after_leader(rest: &str) -> Option<(AiKind, String)> {
    let trimmed = rest.trim_start();
    let body = trimmed.strip_prefix("AI")?;

    // The character directly after `AI` decides the kind and must not be an
    // identifier character (so `AID` / `AISLE` are not matched).
    let (kind, remainder) = match body.chars().next() {
        Some('!') => (AiKind::Bang, &body['!'.len_utf8()..]),
        Some('?') => (AiKind::Question, &body['?'.len_utf8()..]),
        Some(c) if c.is_alphanumeric() || c == '_' => return None,
        _ => (AiKind::Ai, body),
    };

    let text = remainder
        .trim_start()
        .trim_start_matches([':', '-'])
        .trim()
        .to_string();
    Some((kind, text))
}

/// Scans the in-memory contents of a file for AI-trigger comments.
///
/// Each matching line yields one [`AiComment`] whose `line` field is the
/// one-based line number. This function is pure and performs no I/O.
#[must_use]
pub fn scan_text(file: &str, contents: &str) -> Vec<AiComment> {
    contents
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            parse_line(line).map(|(kind, text)| AiComment {
                file: file.to_string(),
                // Line numbers are one-based; `idx` is bounded by the file
                // length and is therefore well within `u32` for real sources.
                line: line_number(idx),
                kind,
                text,
            })
        })
        .collect()
}

/// Converts a zero-based line index into a saturating one-based `u32`.
const fn line_number(idx: usize) -> u32 {
    let one_based = idx.saturating_add(1);
    if one_based > u32::MAX as usize {
        u32::MAX
    } else {
        #[allow(clippy::cast_possible_truncation)]
        {
            one_based as u32
        }
    }
}

/// Walks `cfg.root` and scans every file whose extension matches `cfg`.
///
/// Files are read as UTF-8 lossily so binary or non-UTF-8 files do not abort
/// the scan. Results preserve directory-walk order.
///
/// # Errors
///
/// Returns [`WatchError::Io`] if the directory tree cannot be traversed.
/// Individual files that fail to read are skipped rather than aborting.
pub fn scan_dir(cfg: &ScanConfig) -> Result<Vec<AiComment>, WatchError> {
    let mut out = Vec::new();
    for entry in WalkDir::new(&cfg.root) {
        let entry = entry.map_err(|e| WatchError::Io(e.to_string()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !matches_extension(path, &cfg.extensions) {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let contents = String::from_utf8_lossy(&bytes);
        let name = path.to_string_lossy().into_owned();
        out.extend(scan_text(&name, &contents));
    }
    Ok(out)
}

/// Returns `true` when `path` has one of the configured extensions.
fn matches_extension(path: &std::path::Path, extensions: &[String]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| extensions.iter().any(|want| want.eq_ignore_ascii_case(ext)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn detects_all_kinds_with_slash_leader() {
        assert_eq!(
            parse_line("let x = 1; // AI: rename this"),
            Some((AiKind::Ai, "rename this".to_string()))
        );
        assert_eq!(
            parse_line("foo() // AI! fix now"),
            Some((AiKind::Bang, "fix now".to_string()))
        );
        assert_eq!(
            parse_line("bar() // AI? what does this do"),
            Some((AiKind::Question, "what does this do".to_string()))
        );
    }

    #[test]
    fn detects_hash_and_dash_leaders() {
        assert_eq!(
            parse_line("x = 1  # AI: python style"),
            Some((AiKind::Ai, "python style".to_string()))
        );
        assert_eq!(
            parse_line("SELECT 1 -- AI! sql trigger"),
            Some((AiKind::Bang, "sql trigger".to_string()))
        );
    }

    #[test]
    fn ignores_non_ai_comments_and_lookalikes() {
        assert_eq!(parse_line("// just a normal comment"), None);
        assert_eq!(parse_line("let aircraft = 1; // AID kit"), None);
        assert_eq!(parse_line("// AISLE seat"), None);
        assert_eq!(parse_line("no comment here at all"), None);
    }

    #[test]
    fn bare_marker_has_empty_text() {
        assert_eq!(parse_line("// AI"), Some((AiKind::Ai, String::new())));
        assert_eq!(parse_line("# AI!"), Some((AiKind::Bang, String::new())));
    }

    #[test]
    fn scan_text_reports_correct_line_numbers() {
        let src = "line one\n// AI: second line marker\nline three\n   // AI? fourth\n";
        let found = scan_text("foo.rs", src);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].line, 2);
        assert_eq!(found[0].kind, AiKind::Ai);
        assert_eq!(found[0].file, "foo.rs");
        assert_eq!(found[1].line, 4);
        assert_eq!(found[1].kind, AiKind::Question);
        assert_eq!(found[1].text, "fourth");
    }

    #[test]
    fn ai_kind_mapping_round_trips() {
        let cases = [
            ("// AI go", AiKind::Ai),
            ("// AI! go", AiKind::Bang),
            ("// AI? go", AiKind::Question),
        ];
        for (line, expected) in cases {
            assert_eq!(parse_line(line).unwrap().0, expected);
        }
    }

    #[test]
    fn scan_dir_walks_mixed_files_in_temp_dir() {
        let base = std::env::temp_dir().join(format!("origin-watch-test-{}", std::process::id()));
        let sub = base.join("nested");
        std::fs::create_dir_all(&sub).unwrap();

        std::fs::write(base.join("a.rs"), "fn main() {} // AI: rust marker\n").unwrap();
        std::fs::write(sub.join("b.py"), "x = 1  # AI! py marker\nplain\n").unwrap();
        // Wrong extension: must be ignored.
        std::fs::write(base.join("c.txt"), "// AI: ignored\n").unwrap();
        // No marker: contributes nothing.
        std::fs::write(sub.join("d.rs"), "fn other() {}\n").unwrap();

        let cfg = ScanConfig {
            root: base.to_string_lossy().into_owned(),
            extensions: vec!["rs".to_string(), "py".to_string()],
        };
        let mut found = scan_dir(&cfg).unwrap();
        found.sort_by(|a, b| a.text.cmp(&b.text));

        std::fs::remove_dir_all(&base).unwrap();

        assert_eq!(found.len(), 2);
        let texts: Vec<&str> = found.iter().map(|c| c.text.as_str()).collect();
        assert!(texts.contains(&"rust marker"));
        assert!(texts.contains(&"py marker"));
        assert!(found.iter().any(|c| c.kind == AiKind::Bang));
    }

    #[test]
    fn scan_dir_missing_root_errors() {
        let cfg = ScanConfig {
            root: "this/path/should/not/exist/origin-watch".to_string(),
            extensions: vec!["rs".to_string()],
        };
        assert!(matches!(scan_dir(&cfg), Err(WatchError::Io(_))));
    }
}
