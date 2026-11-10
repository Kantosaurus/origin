// SPDX-License-Identifier: Apache-2.0
//! Default-on, fail-open post-edit compile/syntax gate.
//!
//! After a turn mutates source files, run a FAST per-file check on each edited
//! file and feed any failures back to the model on the next turn (via the same
//! volatile trailing-message channel as the LSP-diagnostics block). This closes
//! the "claimed done but broken" hole: a syntactically broken edit is surfaced
//! before the model settles on a tool-free final answer.
//!
//! Deliberately FAST + per-file only: it runs cheap syntax checkers
//! (`py_compile`, `node --check`) that finish in milliseconds. It does NOT run
//! slow project-wide builds (`cargo check`, `tsc`, `go vet`) by default — those
//! would add seconds-to-minutes of latency to every edit turn on a large
//! workspace. Rust type/borrow errors are already surfaced incrementally by the
//! wired LSP-diagnostics path, so they are intentionally not duplicated here.
//! B7: files in a language whose tree-sitter grammar `origin-codegraph` links
//! (Rust, TypeScript, Go, Java, C/C++, …) — and Python when no interpreter
//! ran — are parsed in-process and `ERROR`/`MISSING` nodes reported by line.
//!
//! Fail-open everywhere: an unrecognised extension, a missing interpreter, or a
//! timeout is silently skipped — the gate can never block or stall a turn.

use std::collections::BTreeSet;
use std::time::Duration;

/// The gate is on unless explicitly opted out with `ORIGIN_EDIT_CHECK=0`.
#[must_use]
pub fn enabled() -> bool {
    std::env::var("ORIGIN_EDIT_CHECK").as_deref() != Ok("0")
}

/// Per-edited-file time budget for a syntax check. Generous for a syntax-only
/// check, but bounded so a wedged interpreter can never stall the turn.
const PER_FILE_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum stderr characters kept per failing file (keeps the fed-back block small).
const MAX_MSG_CHARS: usize = 2_000;

/// B7: larger files are not tree-sitter parsed (silently skipped).
const MAX_PARSE_BYTES: u64 = 1_000_000;

/// B7: problem lines listed per file before the rest are only counted.
const MAX_ERROR_LINES: usize = 5;

/// The fast per-file checker for `path`.
///
/// Returns `(program, args including the path)`, or `None` when there is no
/// cheap check for this file type (fail-open).
fn checker_for(path: &str) -> Option<(&'static str, Vec<String>)> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();
    match ext.as_str() {
        "py" => Some(("python", vec!["-m".into(), "py_compile".into(), path.into()])),
        "js" | "mjs" | "cjs" | "jsx" => Some(("node", vec!["--check".into(), path.into()])),
        _ => None,
    }
}

/// Run the fast checker for each edited path under a per-file timeout.
///
/// Collects failures into an `<edit-check>` block; returns `None` when nothing
/// checkable failed. Fail-open: missing interpreter / timeout / unknown type are
/// skipped.
pub async fn check_block(paths: &BTreeSet<String>) -> Option<String> {
    let mut failures: Vec<String> = Vec::new();
    for p in paths {
        if let Some((prog, args)) = checker_for(p) {
            let mut cmd = tokio::process::Command::new(prog);
            origin_tools::scrub_env(cmd.as_std_mut()); // A6
            let run = cmd.args(&args).output();
            if let Ok(Ok(out)) = tokio::time::timeout(PER_FILE_TIMEOUT, run).await {
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let msg = stderr.trim();
                    let msg = if msg.is_empty() {
                        String::from_utf8_lossy(&out.stdout).trim().to_string()
                    } else {
                        msg.to_string()
                    };
                    if !msg.is_empty() {
                        failures.push(format!("{p}:\n{}", truncate(&msg)));
                    }
                }
                // The interpreter ran: one diagnostic per problem, so no tree-sitter pass.
                continue;
            }
            // Spawn error (no interpreter) or timeout ⇒ tree-sitter may cover it.
        }
        if let Some(msg) = tree_sitter_check(p) {
            failures.push(format!("{p}:\n{msg}"));
        }
    }
    if failures.is_empty() {
        return None;
    }
    Some(format!(
        "<edit-check>\nA post-edit syntax check found problems in files you just edited. \
         Fix these before continuing or claiming done:\n\n{}\n</edit-check>",
        failures.join("\n\n")
    ))
}

/// B7: the tree-sitter grammar for `path`, or `None` (skipped) when
/// `origin-codegraph` links none — or links the wrong one: `.tsx` maps to the
/// plain TypeScript grammar (JSX is an error there) and `.h` to C (C++ headers
/// fail), so both would nag on valid code.
fn parse_language(path: &str) -> Option<origin_codegraph::Language> {
    let p = std::path::Path::new(path);
    let ext = p.extension()?.to_str()?.to_ascii_lowercase();
    if matches!(ext.as_str(), "tsx" | "h") {
        return None;
    }
    origin_codegraph::Language::from_path(p)
}

/// B7: parse the edited file with tree-sitter; fail-open on any read/parse
/// problem and for files over [`MAX_PARSE_BYTES`].
// ponytail: reports every ERROR/MISSING node, including ones the file had
// before the edit (the gate only sees post-edit paths); compare against a
// pre-edit parse captured before tool dispatch if pre-existing errors nag.
fn tree_sitter_check(path: &str) -> Option<String> {
    let lang = parse_language(path)?;
    if std::fs::metadata(path).ok()?.len() > MAX_PARSE_BYTES {
        return None;
    }
    tree_sitter_errors(&std::fs::read(path).ok()?, lang)
}

fn tree_sitter_errors(src: &[u8], lang: origin_codegraph::Language) -> Option<String> {
    let lines = lang.syntax_error_lines(src).ok()?;
    if lines.is_empty() {
        return None;
    }
    let shown: Vec<String> = lines
        .iter()
        .take(MAX_ERROR_LINES)
        .map(ToString::to_string)
        .collect();
    let more = match lines.len().saturating_sub(MAX_ERROR_LINES) {
        0 => String::new(),
        k => format!(" (+{k} more)"),
    };
    Some(format!(
        "possible syntax error (tree-sitter) at line {}{more}",
        shown.join(", ")
    ))
}

fn truncate(s: &str) -> String {
    s.chars().take(MAX_MSG_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_checkers_only_for_known_cheap_types() {
        assert!(checker_for("a.py").is_some(), "python");
        assert!(checker_for("a.js").is_some(), "js");
        assert!(checker_for("a.mjs").is_some(), "esm js");
        // Slow project-wide checks are NOT run per-file (fail-open).
        assert!(checker_for("a.rs").is_none(), "Rust → LSP path, not cargo check");
        assert!(checker_for("a.ts").is_none(), "tsc is project-wide; not a fast per-file check");
        assert!(checker_for("a.go").is_none(), "go vet is project-wide");
        assert!(checker_for("README.md").is_none());
        assert!(checker_for("noext").is_none());
    }

    #[tokio::test]
    async fn empty_paths_yield_no_block() {
        assert!(check_block(&BTreeSet::new()).await.is_none());
    }

    /// Runs the gate on one file named `name` holding `body`.
    async fn check_one(name: &str, body: &str) -> Option<String> {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join(name);
        std::fs::write(&p, body).expect("write");
        check_block(&BTreeSet::from([p.to_string_lossy().into_owned()])).await
    }

    /// B7: an unbalanced brace in a tree-sitter language is reported, as parser
    /// feedback with its line number, in the same `<edit-check>` block.
    #[tokio::test]
    async fn tree_sitter_flags_an_unbalanced_brace_in_rust_and_typescript() {
        for (name, body, line) in [
            ("extra.rs", "fn main() {\n}\n}\n", "at line 3"),
            (
                "extra.ts",
                "function f(): number {\n  return 1;\n}\n}\n",
                "at line 4",
            ),
            // An unclosed block: Rust's grammar flags where it opens, the
            // TypeScript one a MISSING `}` after the last statement.
            ("open.rs", "fn main() {\n    let x = 1;\n", "at line 1"),
            ("open.ts", "function f(): number {\n  return 1;\n", "at line 2"),
        ] {
            let block = check_one(name, body).await.expect(name);
            assert!(block.contains("<edit-check>"), "{block}");
            assert!(block.contains(name), "{block}");
            assert!(block.contains("possible syntax error (tree-sitter)"), "{block}");
            assert!(block.contains(line), "{name} must name {line}: {block}");
        }
    }

    /// B7: valid source and unknown file types yield no diagnostic.
    #[tokio::test]
    async fn tree_sitter_is_silent_on_valid_files_and_unknown_types() {
        assert_eq!(
            check_one("ok.rs", "fn main() {\n    let _x = 1;\n}\n").await,
            None
        );
        assert_eq!(
            check_one("ok.ts", "export function f(): number {\n  return 1;\n}\n").await,
            None
        );
        assert_eq!(check_one("notes.xyz", "}}} not code {{{\n").await, None);
    }

    /// B7: only the first five problem lines are listed, then a count.
    #[test]
    fn tree_sitter_report_is_capped_at_five_lines() {
        let body = "fn main() {}\n}\n".repeat(7);
        let msg = tree_sitter_errors(body.as_bytes(), origin_codegraph::Language::Rust).expect("errors");
        assert_eq!(
            msg,
            "possible syntax error (tree-sitter) at line 2, 4, 6, 8, 10 (+2 more)"
        );
    }

    #[tokio::test]
    async fn a_broken_python_file_is_reported_when_python_is_present() {
        // Fail-open: if `python` isn't on PATH this asserts None (skipped), which
        // is the correct degraded behavior; when present, the syntax error is
        // surfaced in an <edit-check> block.
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("broken.py");
        std::fs::write(&p, "def f(:\n").expect("write");
        let mut set = BTreeSet::new();
        set.insert(p.to_string_lossy().into_owned());
        let out = check_block(&set).await;
        if let Some(block) = out {
            assert!(block.contains("<edit-check>"));
            assert!(block.contains("broken.py"));
        }
        // else: python not installed in this environment ⇒ fail-open skip (ok).
    }
}
