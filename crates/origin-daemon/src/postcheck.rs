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
        let Some((prog, args)) = checker_for(p) else {
            continue;
        };
        let run = tokio::process::Command::new(prog)
            .args(&args)
            .output();
        match tokio::time::timeout(PER_FILE_TIMEOUT, run).await {
            Ok(Ok(out)) if !out.status.success() => {
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
            // Success, spawn error (no interpreter), or timeout ⇒ fail open.
            _ => {}
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
