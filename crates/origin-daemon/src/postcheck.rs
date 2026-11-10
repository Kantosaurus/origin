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
//! ran — are parsed in-process and `ERROR`/`MISSING` nodes reported by line,
//! but only when the edit raised their count over a pre-edit baseline, and as
//! an advisory (`<syntax-advisory>`), not a must-fix: the pinned grammars lag
//! the languages.
//!
//! Fail-open everywhere: an unrecognised extension, a missing interpreter, or a
//! timeout is silently skipped — the gate can never block or stall a turn.

use std::collections::{BTreeSet, HashMap};
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
///
/// The path is passed absolute (resolved against the process cwd, as the edit
/// tools resolve an unanchored path), so a file name is never read as an
/// option. Python runs isolated (`-I`: no cwd or script dir on `sys.path`, no
/// `PYTHON*` variables, no user site), so a `py_compile.py` in the workspace is
/// never imported. `node --check` only parses (no `--require`/preload comes
/// from anywhere but `NODE_OPTIONS`, which the env scrub drops); the nearest
/// `package.json` is read for the module type only.
fn checker_for(path: &str) -> Option<(&'static str, Vec<String>)> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();
    let abs = || {
        std::path::absolute(path)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    };
    match ext.as_str() {
        "py" => Some((
            "python",
            vec!["-I".into(), "-m".into(), "py_compile".into(), abs()?],
        )),
        // Not `.jsx`: `node --check` rejects JSX (B7).
        "js" | "mjs" | "cjs" => Some(("node", vec!["--check".into(), abs()?])),
        _ => None,
    }
}

/// The checker's process: scrubbed env (A6), run from the temp directory so
/// nothing in the workspace is on the interpreter's module path (S1).
fn checker_command<S: AsRef<std::ffi::OsStr>>(prog: S, args: &[String]) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(prog);
    origin_tools::scrub_env(cmd.as_std_mut()); // A6
    cmd.args(args).current_dir(std::env::temp_dir());
    cmd
}

/// B7: pre-edit tree-sitter problem-line count per edited path (see
/// [`record_baseline`]); a path missing from it counts 0.
pub type ParseBaseline = HashMap<String, usize>;

/// One edited file's finding.
#[derive(Debug, PartialEq, Eq)]
enum Finding {
    /// The interpreter-based checker failed: a real syntax error.
    Failure(String),
    /// B7: tree-sitter found more problem lines than before the edit (advisory).
    Advisory(String),
}

/// Run the fast checks for each edited path.
///
/// Returns `(edit_check, syntax_advisory)`: an `<edit-check>` block of
/// interpreter failures (the model must fix them) and a `<syntax-advisory>`
/// block of tree-sitter notes (may be a parser limitation); each is empty when
/// it has nothing to say. Fail-open: missing interpreter / timeout / unknown
/// type / a file that no longer exists (deleted or moved by `ApplyPatch`) are
/// skipped.
pub async fn check_block(paths: &BTreeSet<String>, baseline: &ParseBaseline) -> (String, String) {
    let mut failures: Vec<String> = Vec::new();
    let mut advisories: Vec<String> = Vec::new();
    for p in paths {
        if !std::path::Path::new(p).exists() {
            continue; // B8: deleted or moved away by ApplyPatch
        }
        let before = baseline.get(p).copied().unwrap_or(0);
        match check_file(p, checker_for(p), before).await {
            Some(Finding::Failure(msg)) => failures.push(format!("{p}:\n{msg}")),
            Some(Finding::Advisory(msg)) => advisories.push(format!("{p}:\n{msg}")),
            None => {}
        }
    }
    let edit_check = if failures.is_empty() {
        String::new()
    } else {
        format!(
            "<edit-check>\nA post-edit syntax check found problems in files you just edited. \
             Fix these before continuing or claiming done:\n\n{}\n</edit-check>",
            failures.join("\n\n")
        )
    };
    let advisory = if advisories.is_empty() {
        String::new()
    } else {
        format!(
            "<syntax-advisory>\nA tree-sitter parse found new parse errors in files you just edited:\n\n{}\n\
             </syntax-advisory>",
            advisories.join("\n\n")
        )
    };
    (edit_check, advisory)
}

/// Check one edited file: the interpreter-based `checker` when it can run,
/// else (B7) tree-sitter, reported only when the edit raised the problem-line
/// count above `before`.
async fn check_file<S: AsRef<std::ffi::OsStr>>(
    path: &str,
    checker: Option<(S, Vec<String>)>,
    before: usize,
) -> Option<Finding> {
    if let Some((prog, args)) = checker {
        let run = checker_command(prog, &args).output();
        if let Ok(Ok(out)) = tokio::time::timeout(PER_FILE_TIMEOUT, run).await {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if !interpreter_missing(out.status.code(), &stderr) {
                // The interpreter ran: one diagnostic per problem, so no tree-sitter pass.
                if out.status.success() {
                    return None;
                }
                let msg = stderr.trim();
                let msg = if msg.is_empty() {
                    String::from_utf8_lossy(&out.stdout).trim().to_string()
                } else {
                    msg.to_string()
                };
                return (!msg.is_empty()).then(|| Finding::Failure(truncate(&msg)));
            }
        }
        // Spawn error, timeout or a stub interpreter ⇒ tree-sitter may cover it.
    }
    let lines = parse_error_lines(path).await;
    (lines.len() > before).then(|| Finding::Advisory(advisory_text(&lines)))
}

/// B7: the run produced no check at all. The Windows Store `python` "App
/// Execution Alias" (installed by default) runs, exits 9009 and prints
/// `Python was not found; run without arguments to install from the Microsoft
/// Store, …` — no interpreter, not a syntax error. S1: an interpreter that
/// rejects the isolation flag (`Unknown option: -I`) did not check either.
fn interpreter_missing(code: Option<i32>, stderr: &str) -> bool {
    let first = stderr.trim_start();
    code == Some(9009)
        || first.starts_with("Python was not found")
        || (code != Some(0) && first.to_ascii_lowercase().starts_with("unknown option"))
}

/// B7: record `path`'s pre-edit tree-sitter problem-line count in `baseline`.
///
/// Call before the tool that edits `path` runs; the first call per path wins
/// (a file that does not exist yet counts 0). No-op for files tree-sitter
/// does not check.
pub async fn record_baseline(baseline: &mut ParseBaseline, path: &str) {
    if baseline.contains_key(path) || parse_language(path).is_none() {
        return;
    }
    let count = parse_error_lines(path).await.len();
    baseline.insert(path.to_string(), count);
}

/// B8: record a moved file's baseline at its destination `to`.
///
/// It is `from`'s pre-edit count, so problems the file already had are not
/// reported as new at its new path. Same first-call-wins rule as
/// [`record_baseline`].
pub async fn record_moved_baseline(baseline: &mut ParseBaseline, from: &str, to: &str) {
    if baseline.contains_key(to) || parse_language(to).is_none() {
        return;
    }
    let count = parse_error_lines(from).await.len();
    baseline.insert(to.to_string(), count);
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

/// B7: the lines of tree-sitter `ERROR`/`MISSING` nodes in `path`, read and
/// parsed off the async runtime under [`PER_FILE_TIMEOUT`]. Empty (fail-open)
/// for unchecked types, files over [`MAX_PARSE_BYTES`], missing files, and on
/// any read/parse error or timeout.
async fn parse_error_lines(path: &str) -> Vec<usize> {
    let Some(lang) = parse_language(path) else {
        return Vec::new();
    };
    let path = path.to_string();
    let parse = tokio::task::spawn_blocking(move || {
        if std::fs::metadata(&path).map_or(true, |m| m.len() > MAX_PARSE_BYTES) {
            return Vec::new();
        }
        std::fs::read(&path)
            .ok()
            .and_then(|src| lang.syntax_error_lines(&src).ok())
            .unwrap_or_default()
    });
    match tokio::time::timeout(PER_FILE_TIMEOUT, parse).await {
        Ok(Ok(lines)) => lines,
        _ => Vec::new(),
    }
}

/// B7: the advisory note for a file's tree-sitter problem `lines`.
fn advisory_text(lines: &[usize]) -> String {
    let shown: Vec<String> = lines
        .iter()
        .take(MAX_ERROR_LINES)
        .map(ToString::to_string)
        .collect();
    let more = match lines.len().saturating_sub(MAX_ERROR_LINES) {
        0 => String::new(),
        k => format!(" (+{k} more)"),
    };
    format!(
        "possible syntax error (tree-sitter) at line {}{more}; may be a parser limitation — ignore if \
         the code builds",
        shown.join(", ")
    )
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

    /// S1: every checker gets an absolute path and runs from the temp
    /// directory; Python runs isolated (`-I` before `-m`).
    #[test]
    fn checkers_run_isolated_from_the_workspace() {
        for (file, prog) in [
            ("x.py", "python"),
            ("-x.js", "node"),
            ("x.mjs", "node"),
            ("x.cjs", "node"),
        ] {
            let (got, args) = checker_for(file).expect("checker");
            assert_eq!(got, prog);
            let path = args.last().expect("path");
            assert!(std::path::Path::new(path).is_absolute(), "{file}: {path}");
            let cmd = checker_command(got, &args);
            assert_eq!(
                cmd.as_std().get_current_dir(),
                Some(std::env::temp_dir().as_path()),
                "{file}"
            );
        }
        let (_, args) = checker_for("x.py").expect("python");
        let at = |a: &str| args.iter().position(|x| x == a);
        assert!(at("-I").expect("-I") < at("-m").expect("-m"), "{args:?}");
    }

    /// S1: an interpreter that rejects `-I` counts as missing (tree-sitter
    /// covers), never as a syntax error.
    #[test]
    fn a_rejected_isolation_flag_counts_as_a_missing_interpreter() {
        assert!(interpreter_missing(
            Some(2),
            "Unknown option: -I\nusage: python [option] ...\n"
        ));
        assert!(interpreter_missing(Some(2), "unknown option -I\n"));
        assert!(!interpreter_missing(Some(0), "unknown option -I\n"));
    }

    #[tokio::test]
    async fn empty_paths_yield_no_block() {
        let none = (String::new(), String::new());
        assert_eq!(check_block(&BTreeSet::new(), &ParseBaseline::new()).await, none);
    }

    /// Runs the gate on a new file (no baseline) named `name` holding `body`;
    /// returns `(edit_check, syntax_advisory)`.
    async fn check_one(name: &str, body: &str) -> (String, String) {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join(name);
        std::fs::write(&p, body).expect("write");
        check_block(
            &BTreeSet::from([p.to_string_lossy().into_owned()]),
            &ParseBaseline::new(),
        )
        .await
    }

    /// B7: an unbalanced brace in a tree-sitter language is reported with its
    /// line number, as an advisory outside the must-fix `<edit-check>` block.
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
            let (edit_check, advisory) = check_one(name, body).await;
            assert_eq!(edit_check, "", "{name}: tree-sitter is never a must-fix");
            assert!(advisory.starts_with("<syntax-advisory>"), "{name}: {advisory}");
            assert!(advisory.contains(name), "{advisory}");
            assert!(
                advisory.contains("possible syntax error (tree-sitter)"),
                "{advisory}"
            );
            assert!(advisory.contains("ignore if the code builds"), "{advisory}");
            assert!(!advisory.contains("Fix these"), "{advisory}");
            assert!(advisory.contains(line), "{name} must name {line}: {advisory}");
        }
    }

    /// B7: valid source, unknown file types, `.tsx`/`.h` (wrong grammar) and
    /// files over 1 MB yield nothing.
    #[tokio::test]
    async fn tree_sitter_is_silent_on_valid_unknown_skipped_and_large_files() {
        let none = (String::new(), String::new());
        assert_eq!(
            check_one("ok.rs", "fn main() {\n    let _x = 1;\n}\n").await,
            none
        );
        assert_eq!(
            check_one("ok.ts", "export function f(): number {\n  return 1;\n}\n").await,
            none
        );
        for name in ["notes.xyz", "view.tsx", "api.h"] {
            assert_eq!(check_one(name, "}}} not code {{{\n").await, none, "{name}");
        }
        let large = "}\n".repeat(600_000);
        assert_eq!(check_one("large.rs", &large).await, none);
    }

    /// B7: `node --check` rejects `.jsx` (JSX syntax, and node 24 refuses the
    /// extension outright), and no JavaScript tree-sitter grammar is linked, so
    /// `.jsx` is skipped silently — valid or broken.
    #[tokio::test]
    async fn jsx_is_skipped_silently() {
        assert!(checker_for("App.jsx").is_none());
        assert!(parse_language("App.jsx").is_none(), "no JS grammar linked");
        let none = (String::new(), String::new());
        let valid = "export function App() {\n  return <div className=\"app\">hi</div>;\n}\n";
        assert_eq!(check_one("App.jsx", valid).await, none);
        assert_eq!(check_one("Broken.jsx", "export function App() {\n").await, none);
    }

    /// B7: only the first five problem lines are listed, then a count.
    #[test]
    fn tree_sitter_report_is_capped_at_five_lines() {
        assert_eq!(
            advisory_text(&[2, 4, 6, 8, 10, 12, 14]),
            "possible syntax error (tree-sitter) at line 2, 4, 6, 8, 10 (+2 more); may be a parser \
             limitation — ignore if the code builds"
        );
    }

    /// Edits `path` from `before` (`None`: the file does not exist yet) to
    /// `after` with the baseline recorded in between, as the agent loop does.
    async fn edit(path: &std::path::Path, before: Option<&str>, after: &str) -> String {
        if let Some(body) = before {
            std::fs::write(path, body).expect("write");
        }
        let p = path.to_string_lossy().into_owned();
        let mut baseline = ParseBaseline::new();
        record_baseline(&mut baseline, &p).await;
        std::fs::write(path, after).expect("write");
        check_block(&BTreeSet::from([p]), &baseline).await.1
    }

    /// B7: only parse errors the edit ADDED are reported — a file that already
    /// had an unbalanced brace does not nag on every later edit.
    #[tokio::test]
    async fn tree_sitter_reports_only_errors_the_edit_added() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("lib.rs");
        let broken = "fn a() {}\n}\n";
        // Pre-existing error, the edit adds a valid fn: silent.
        let unchanged = edit(&p, Some(broken), "fn a() {}\n}\nfn b() {}\n").await;
        assert_eq!(unchanged, "");
        // The edit adds a second stray brace: reported.
        let added = edit(&p, Some(broken), "fn a() {}\n}\nfn b() {}\n}\n").await;
        assert!(added.contains("possible syntax error (tree-sitter)"), "{added}");
        // A new file with an error: reported (a missing file counts 0).
        let q = dir.path().join("new.rs");
        let new_file = edit(&q, None, broken).await;
        assert!(new_file.contains("new.rs"), "{new_file}");
    }

    /// B7: when the interpreter-based check ran, tree-sitter does not run too
    /// (one diagnostic per problem); when it could not run, tree-sitter covers.
    #[tokio::test]
    async fn tree_sitter_runs_only_when_the_interpreter_check_did_not() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("broken.rs");
        std::fs::write(&p, "fn main() {\n}\n}\n").expect("write");
        let p = p.to_string_lossy().into_owned();
        // This test binary stands in for an interpreter that runs: `--list`
        // exits 0, an unknown flag exits non-zero with a message.
        let exe = std::env::current_exe().expect("current exe");
        let clean = check_file(&p, Some((&exe, vec!["--list".into()])), 0).await;
        assert_eq!(clean, None);
        let failed = check_file(&p, Some((&exe, vec!["--no-such-flag".into()])), 0).await;
        assert!(matches!(failed, Some(Finding::Failure(_))), "{failed:?}");
        let missing = check_file(&p, Some(("origin-no-such-interpreter", Vec::new())), 0).await;
        assert!(matches!(missing, Some(Finding::Advisory(_))), "{missing:?}");
    }

    /// B7: the Windows Store `python` alias (exit 9009, "Python was not
    /// found; …", observed on Windows 11) counts as no interpreter; a real
    /// `py_compile` failure does not.
    #[test]
    fn store_python_alias_counts_as_a_missing_interpreter() {
        let alias = "Python was not found; run without arguments to install from the Microsoft Store, or \
                     disable this shortcut from Settings > Apps > Advanced app settings > App execution aliases.\r\n";
        assert!(interpreter_missing(Some(9009), alias));
        assert!(interpreter_missing(Some(9009), ""));
        assert!(interpreter_missing(Some(1), alias));
        let real = "  File \"x.py\", line 1\n    def f(:\n          ^\nSyntaxError: invalid syntax\n";
        assert!(!interpreter_missing(Some(1), real));
        assert!(!interpreter_missing(Some(0), ""));
    }

    #[tokio::test]
    async fn a_broken_python_file_is_reported_when_python_is_present() {
        // When `python` runs, the syntax error is surfaced in an <edit-check>
        // block; without one (or with only the Store alias), tree-sitter covers.
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("broken.py");
        std::fs::write(&p, "def f(:\n").expect("write");
        let mut set = BTreeSet::new();
        set.insert(p.to_string_lossy().into_owned());
        let (block, advisory) = check_block(&set, &ParseBaseline::new()).await;
        if block.is_empty() {
            // No python here ⇒ tree-sitter covers the file instead.
            assert!(advisory.contains("broken.py"), "{advisory}");
        } else {
            assert!(block.contains("<edit-check>"));
            assert!(block.contains("broken.py"));
            assert_eq!(advisory, "", "one diagnostic per problem");
        }
    }

    /// B8: an `ApplyPatch` delete puts a path that no longer exists in the
    /// edited set; it must not read as a syntax failure.
    #[tokio::test]
    async fn a_deleted_file_is_skipped() {
        let dir = tempfile::tempdir().expect("tmp");
        let mut set = BTreeSet::new();
        set.insert(dir.path().join("gone.py").to_string_lossy().into_owned());
        assert_eq!(
            check_block(&set, &ParseBaseline::new()).await,
            (String::new(), String::new())
        );
    }

    /// B8: a file moved with its pre-existing parse error is not reported as
    /// new at its destination.
    #[tokio::test]
    async fn a_moved_file_keeps_its_baseline() {
        let dir = tempfile::tempdir().expect("tmp");
        let from = dir.path().join("old.rs");
        let to = dir.path().join("new.rs");
        std::fs::write(&from, "fn a() {}\n}\n").expect("write");
        let (from_s, to_s) = (from.to_string_lossy().into_owned(), to.to_string_lossy().into_owned());
        let mut baseline = ParseBaseline::new();
        record_moved_baseline(&mut baseline, &from_s, &to_s).await;
        record_baseline(&mut baseline, &to_s).await;
        std::fs::rename(&from, &to).expect("move");
        assert_eq!(check_block(&BTreeSet::from([to_s]), &baseline).await.1, "");
    }
}
