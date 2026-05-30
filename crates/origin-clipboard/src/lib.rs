// SPDX-License-Identifier: Apache-2.0
//! Copy/paste web-chat mode for origin.
//!
//! Formats a context bundle into a prompt-ready block to paste into a browser
//! chat, and parses an LLM's pasted reply back into structured file edits. The
//! crate is pure logic: reading and writing the OS clipboard is left to the
//! caller, which can use [`os_copy_command`] / [`os_paste_command`] to learn
//! the right shell program for the current platform.

#![forbid(unsafe_code)]

use thiserror::Error;

/// Errors that can occur while building or interpreting clipboard payloads.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ClipboardError {
    /// A pasted block was structurally invalid and could not be parsed.
    #[error("malformed edit block: {0}")]
    Malformed(String),
}

/// A bundle of file contents plus an instruction to send to a web chat.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContextBundle {
    /// The files to include, each as a `(path, contents)` pair.
    pub files: Vec<(String, String)>,
    /// The natural-language instruction for the model.
    pub instruction: String,
}

impl ContextBundle {
    /// Creates a new context bundle from files and an instruction.
    #[must_use]
    pub const fn new(files: Vec<(String, String)>, instruction: String) -> Self {
        Self { files, instruction }
    }
}

/// A single file operation parsed out of a pasted reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditBlock {
    /// Replace the first occurrence of `search` with `replace` in `file`.
    SearchReplace {
        /// Target file path.
        file: String,
        /// Exact text to find.
        search: String,
        /// Replacement text.
        replace: String,
    },
    /// Overwrite `file` entirely with `contents`.
    WholeFile {
        /// Target file path.
        file: String,
        /// Full new contents of the file.
        contents: String,
    },
}

/// Fence marker used to open and close code blocks.
const FENCE: &str = "```";

/// Marker that opens the "search" half of a search/replace block.
const SEARCH_MARKER: &str = "<<<<<<< SEARCH";
/// Marker that divides the search half from the replace half.
const DIVIDER_MARKER: &str = "=======";
/// Marker that closes the "replace" half of a search/replace block.
const REPLACE_MARKER: &str = ">>>>>>> REPLACE";

/// Formats a context bundle into a clean, prompt-ready block to paste.
///
/// Each file is rendered inside a fenced code block whose info string carries
/// the file path, followed by the instruction at the end. The output is
/// deterministic so it can be diffed and tested.
#[must_use]
pub fn format_for_paste(b: &ContextBundle) -> String {
    let mut out = String::new();
    for (path, contents) in &b.files {
        out.push_str("File: ");
        out.push_str(path);
        out.push('\n');
        out.push_str(FENCE);
        out.push_str(language_hint(path));
        out.push('\n');
        out.push_str(contents);
        if !contents.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(FENCE);
        out.push_str("\n\n");
    }
    out.push_str("Instruction:\n");
    out.push_str(&b.instruction);
    if !b.instruction.is_empty() && !b.instruction.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Returns a fence language hint derived from a file extension, or empty.
#[must_use]
fn language_hint(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("rs") => "rust",
        Some("py") => "python",
        Some("js" | "mjs" | "cjs") => "javascript",
        Some("ts") => "typescript",
        Some("go") => "go",
        Some("toml") => "toml",
        Some("json") => "json",
        Some("md") => "markdown",
        Some("sh") => "bash",
        _ => "",
    }
}

/// Parses the two common LLM edit formats out of a pasted reply.
///
/// Recognises aider-style search/replace blocks (a fenced block containing
/// `<<<<<<< SEARCH` / `=======` / `>>>>>>> REPLACE`) and full-file blocks (a
/// fenced block immediately preceded by a `path:`-style header line). Prose
/// between blocks is ignored. Returns the edits in the order they appear.
///
/// Blocks that open a fence but never close it, or that are missing required
/// markers, are skipped rather than reported, so partial pastes degrade
/// gracefully into the edits that *were* well-formed.
#[must_use]
pub fn parse_pasted_edits(text: &str) -> Vec<EditBlock> {
    let lines: Vec<&str> = text.lines().collect();
    let mut edits = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if is_fence_open(line) {
            let header_path = preceding_path(&lines, i);
            if let Some((block, next)) = take_fenced_block(&lines, i) {
                if let Some(edit) = interpret_block(&block, header_path.as_deref()) {
                    edits.push(edit);
                }
                i = next;
                continue;
            }
        }
        i += 1;
    }
    edits
}

/// Returns true if a line opens (or closes) a code fence.
#[must_use]
fn is_fence_open(line: &str) -> bool {
    line.trim_start().starts_with(FENCE)
}

/// Looks backwards from a fence for a `path:`/`File:` header line.
///
/// Skips a single blank line between the header and the fence, mirroring how
/// chat models often format full-file answers.
#[must_use]
fn preceding_path(lines: &[&str], fence_idx: usize) -> Option<String> {
    let mut j = fence_idx;
    while j > 0 {
        j -= 1;
        let candidate = lines[j].trim();
        if candidate.is_empty() {
            continue;
        }
        return extract_path_header(candidate);
    }
    None
}

/// Extracts a path from a header line such as `path: src/a.rs` or `File: a.rs`.
fn extract_path_header(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    for prefix in ["path:", "file:", "filename:"] {
        if lower.starts_with(prefix) {
            let value = line[prefix.len()..].trim();
            let value = value.trim_matches('`').trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Collects the lines inside a fenced block, returning them and the index past
/// the closing fence. Returns `None` if the block is never closed.
fn take_fenced_block<'a>(lines: &[&'a str], open_idx: usize) -> Option<(Vec<&'a str>, usize)> {
    let mut body = Vec::new();
    let mut i = open_idx + 1;
    while i < lines.len() {
        if is_fence_open(lines[i]) {
            return Some((body, i + 1));
        }
        body.push(lines[i]);
        i += 1;
    }
    None
}

/// Turns a fenced block body into an [`EditBlock`], if it forms a valid edit.
fn interpret_block(body: &[&str], header_path: Option<&str>) -> Option<EditBlock> {
    if let Some(edit) = parse_search_replace(body, header_path) {
        return Some(edit);
    }
    header_path.map(|file| EditBlock::WholeFile {
        file: file.to_string(),
        contents: join_with_trailing_newline(body),
    })
}

/// Parses an aider-style search/replace body if all three markers are present.
fn parse_search_replace(body: &[&str], header_path: Option<&str>) -> Option<EditBlock> {
    let search_idx = body.iter().position(|l| l.trim() == SEARCH_MARKER)?;
    let divider_idx = body
        .iter()
        .skip(search_idx + 1)
        .position(|l| l.trim() == DIVIDER_MARKER)
        .map(|p| p + search_idx + 1)?;
    let replace_idx = body
        .iter()
        .skip(divider_idx + 1)
        .position(|l| l.trim() == REPLACE_MARKER)
        .map(|p| p + divider_idx + 1)?;

    // A path may sit on the line just above the SEARCH marker, else use header.
    let inline_path = if search_idx > 0 {
        let candidate = body[search_idx - 1].trim();
        if candidate.is_empty() {
            None
        } else {
            Some(candidate.to_string())
        }
    } else {
        None
    };
    let file = inline_path.or_else(|| header_path.map(ToString::to_string))?;

    let search = join_with_trailing_newline(&body[search_idx + 1..divider_idx]);
    let replace = join_with_trailing_newline(&body[divider_idx + 1..replace_idx]);
    Some(EditBlock::SearchReplace {
        file,
        search,
        replace,
    })
}

/// Joins lines with `\n`, appending a trailing newline when non-empty.
fn join_with_trailing_newline(lines: &[&str]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

/// Returns the platform command that writes stdin to the clipboard.
///
/// The program and its arguments are returned separately so a caller can spawn
/// it directly (for example with [`std::process::Command`]). On macOS this is
/// `pbcopy`, on Windows `clip`, and on other Unix-likes `xclip -selection
/// clipboard`.
#[must_use]
pub fn os_copy_command() -> (&'static str, Vec<String>) {
    if cfg!(target_os = "macos") {
        ("pbcopy", Vec::new())
    } else if cfg!(target_os = "windows") {
        ("clip", Vec::new())
    } else {
        ("xclip", vec!["-selection".to_string(), "clipboard".to_string()])
    }
}

/// Returns the platform command that prints the clipboard to stdout.
///
/// On macOS this is `pbpaste`, on Windows `powershell Get-Clipboard`, and on
/// other Unix-likes `xclip -selection clipboard -o`.
#[must_use]
pub fn os_paste_command() -> (&'static str, Vec<String>) {
    if cfg!(target_os = "macos") {
        ("pbpaste", Vec::new())
    } else if cfg!(target_os = "windows") {
        (
            "powershell",
            vec!["-NoProfile".to_string(), "-Command".to_string(), "Get-Clipboard".to_string()],
        )
    } else {
        (
            "xclip",
            vec![
                "-selection".to_string(),
                "clipboard".to_string(),
                "-o".to_string(),
            ],
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn format_for_paste_fences_files_and_includes_instruction() {
        let bundle = ContextBundle::new(
            vec![("src/main.rs".to_string(), "fn main() {}".to_string())],
            "Add a greeting.".to_string(),
        );
        let out = format_for_paste(&bundle);
        assert!(out.contains("File: src/main.rs"));
        assert!(out.contains("```rust"));
        assert!(out.contains("fn main() {}"));
        assert!(out.contains("Instruction:"));
        assert!(out.contains("Add a greeting."));
        // The file body must be wrapped by an opening and closing fence.
        assert_eq!(out.matches("```").count(), 2);
    }

    #[test]
    fn parse_search_replace_block() {
        let reply = "\
path: src/lib.rs
```
<<<<<<< SEARCH
let x = 1;
=======
let x = 2;
>>>>>>> REPLACE
```";
        let edits = parse_pasted_edits(reply);
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0],
            EditBlock::SearchReplace {
                file: "src/lib.rs".to_string(),
                search: "let x = 1;\n".to_string(),
                replace: "let x = 2;\n".to_string(),
            }
        );
    }

    #[test]
    fn parse_whole_file_block() {
        let reply = "\
File: src/new.rs
```rust
fn added() {}
```";
        let edits = parse_pasted_edits(reply);
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0],
            EditBlock::WholeFile {
                file: "src/new.rs".to_string(),
                contents: "fn added() {}\n".to_string(),
            }
        );
    }

    #[test]
    fn ignores_prose_between_blocks() {
        let reply = "\
Here is some reasoning that should be ignored.

path: a.txt
```
<<<<<<< SEARCH
old
=======
new
>>>>>>> REPLACE
```

And here is more prose with no fence.

path: b.txt
```
hello world
```";
        let edits = parse_pasted_edits(reply);
        assert_eq!(edits.len(), 2);
        assert!(matches!(edits[0], EditBlock::SearchReplace { .. }));
        assert_eq!(
            edits[1],
            EditBlock::WholeFile {
                file: "b.txt".to_string(),
                contents: "hello world\n".to_string(),
            }
        );
    }

    #[test]
    fn unclosed_fence_is_skipped() {
        let reply = "\
path: x.rs
```
this block never closes";
        let edits = parse_pasted_edits(reply);
        assert!(edits.is_empty());
    }

    #[test]
    fn search_replace_without_path_is_skipped() {
        let reply = "\
```
<<<<<<< SEARCH
a
=======
b
>>>>>>> REPLACE
```";
        // No header path and no inline path above SEARCH => cannot target a file.
        let edits = parse_pasted_edits(reply);
        assert!(edits.is_empty());
    }

    #[test]
    fn os_copy_command_returns_non_empty_program() {
        let (prog, _args) = os_copy_command();
        assert!(!prog.is_empty());
    }

    #[test]
    fn os_paste_command_returns_non_empty_program() {
        let (prog, _args) = os_paste_command();
        assert!(!prog.is_empty());
    }

    #[test]
    fn round_trip_on_sample_reply() {
        // Build a bundle, format it, and confirm a model-style reply parses.
        let bundle = ContextBundle::new(
            vec![("src/lib.rs".to_string(), "let x = 1;\n".to_string())],
            "Bump x to 2.".to_string(),
        );
        let prompt = format_for_paste(&bundle);
        assert!(prompt.contains("let x = 1;"));

        let reply = "\
Sure, here is the change.

path: src/lib.rs
```
<<<<<<< SEARCH
let x = 1;
=======
let x = 2;
>>>>>>> REPLACE
```";
        let edits = parse_pasted_edits(reply);
        assert_eq!(
            edits,
            vec![EditBlock::SearchReplace {
                file: "src/lib.rs".to_string(),
                search: "let x = 1;\n".to_string(),
                replace: "let x = 2;\n".to_string(),
            }]
        );
    }

    #[test]
    fn inline_path_above_search_marker_is_used() {
        let reply = "\
```
src/inline.rs
<<<<<<< SEARCH
foo
=======
bar
>>>>>>> REPLACE
```";
        let edits = parse_pasted_edits(reply);
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0],
            EditBlock::SearchReplace {
                file: "src/inline.rs".to_string(),
                search: "foo\n".to_string(),
                replace: "bar\n".to_string(),
            }
        );
    }
}
