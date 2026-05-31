// SPDX-License-Identifier: Apache-2.0
//! Autonomous post-edit LSP diagnostics feedback (opencode "auto-installing LSP
//! servers … feeding diagnostics back").
//!
//! This is the daemon-side complement to the `origin lsp ls`/`ensure` CLI and
//! the [`origin_lspfleet`] registry: after a turn that **mutated** files, the
//! agent loop hands the distinct edited paths here. For each path we resolve its
//! language server from the registry, spawn it through [`origin_lsp_client`],
//! `did_open` the edited file, collect `textDocument/publishDiagnostics` under a
//! short timeout, then drop the client (which reaps the server). The collected
//! diagnostics are rendered into a compact `<lsp-diagnostics>` block that the
//! agent loop appends to the *next* turn's system prompt so the model sees the
//! compiler/linter feedback its edit produced.
//!
//! Everything here is best-effort and **gated behind `ORIGIN_LSP_DIAGNOSTICS=1`**
//! (default off ⇒ the agent loop's per-turn system prompt is byte-identical).
//! Any spawn / timeout / parse failure is swallowed — a flaky language server
//! must never fail the turn that produced the edit.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use origin_lsp_client::{Diagnostic, LspClient};

/// Per-file diagnostics-collection timeout. Kept short: language servers that
/// publish quickly return well before this (the probe returns as soon as the
/// server reports the opened file), while a slow/non-publishing server costs at
/// most this deadline per edited file. The total per-turn cost is bounded by
/// [`MAX_FILES_PER_TURN`] × this value.
const PER_FILE_TIMEOUT: Duration = Duration::from_millis(2500);

/// Upper bound on how many distinct edited files we probe in a single turn.
/// Spawning one short-lived server per file is the dominant cost; capping it
/// keeps a turn that rewrote dozens of files from stalling on diagnostics.
const MAX_FILES_PER_TURN: usize = 8;

/// Maximum number of diagnostic lines rendered into the block, across all files.
/// Bounds the token cost of the injected context regardless of how noisy the
/// servers are; an overflow is summarized with a trailing "… and N more" line.
const MAX_DIAG_LINES: usize = 40;

/// Returns `true` when the autonomous post-edit diagnostics feature is enabled.
///
/// Reads `ORIGIN_LSP_DIAGNOSTICS`; only the exact value `"1"` enables it. With
/// the variable unset (the default) this is `false`, so the agent loop skips the
/// whole routine and the next turn's system prompt is byte-identical to before.
#[must_use]
pub fn enabled() -> bool {
    std::env::var("ORIGIN_LSP_DIAGNOSTICS").as_deref() == Ok("1")
}

/// Map an LSP diagnostic severity code to its short label.
///
/// The wire codes are `1=error`, `2=warning`, `3=info`, `4=hint`; anything else
/// falls back to `warning` (the same defaulting the client's parser uses).
const fn severity_label(severity: u8) -> &'static str {
    match severity {
        1 => "error",
        3 => "info",
        4 => "hint",
        _ => "warning",
    }
}

/// Render one diagnostic as `path:line:col severity message` (1-based line/col).
///
/// LSP positions are zero-based; we render 1-based to match what editors and
/// compilers print, so the model can cross-reference the file it just edited.
/// The message is collapsed to a single line (newlines → spaces) to keep the
/// block compact.
fn render_one(d: &Diagnostic) -> String {
    let line = d.line.saturating_add(1);
    let col = d.col.saturating_add(1);
    let path = d.file.display();
    let sev = severity_label(d.severity);
    let msg = collapse_ws(&d.message);
    match d.code.as_deref() {
        Some(code) if !code.is_empty() => format!("{path}:{line}:{col} {sev}[{code}] {msg}"),
        _ => format!("{path}:{line}:{col} {sev} {msg}"),
    }
}

/// Collapse all runs of ASCII whitespace (including newlines) to single spaces
/// and trim the ends, so a multi-line diagnostic message renders on one line.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Format a set of collected diagnostics into the `<lsp-diagnostics>` block, or
/// `None` when there is nothing actionable to report.
///
/// Pure and order-stable: each diagnostic is rendered to its
/// `path:line:col severity message` line, then the lines are sorted lexically
/// and exact duplicates removed, so the same set always produces the same text
/// (and a server that re-publishes an identical diagnostic collapses to one
/// line). Returns `None` for an empty input (a clean edit surfaces no block —
/// keeping the next turn's prompt unchanged). Output is capped at
/// [`MAX_DIAG_LINES`] lines with an overflow summary.
#[must_use]
pub fn format_diagnostics_block(diags: &[Diagnostic]) -> Option<String> {
    use std::fmt::Write as _;
    if diags.is_empty() {
        return None;
    }
    // Dedup the diagnostics themselves (by their rendered line) so a server that
    // re-publishes an identical diagnostic collapses to one entry, and BOTH the
    // severity summary and the printed lines derive from the same deduped, sorted
    // set. (Counting on the raw input over-counted duplicate diagnostics.)
    let mut deduped: Vec<Diagnostic> = diags.to_vec();
    deduped.sort_by_cached_key(render_one);
    deduped.dedup_by_key(|d| render_one(d));
    if deduped.is_empty() {
        return None;
    }
    let (errors, warnings) = count_severities(&deduped);
    let rendered: Vec<String> = deduped.iter().map(render_one).collect();
    let mut out = String::from(
        "<lsp-diagnostics>\n\
         Language-server diagnostics for the files you just edited. Fix any \
         `error` before continuing; `warning`/`info`/`hint` are advisory.\n",
    );
    let _ = writeln!(out, "summary: {errors} error(s), {warnings} warning(s)");
    let shown = rendered.len().min(MAX_DIAG_LINES);
    for line in rendered.iter().take(shown) {
        out.push_str("- ");
        out.push_str(line);
        out.push('\n');
    }
    if rendered.len() > shown {
        let _ = writeln!(out, "… and {} more", rendered.len() - shown);
    }
    out.push_str("</lsp-diagnostics>");
    Some(out)
}

/// Count error- and warning-severity diagnostics (`(errors, warnings)`).
/// Info/hint are not counted, mirroring [`origin_lspfleet::summary`].
fn count_severities(diags: &[Diagnostic]) -> (u32, u32) {
    let mut errors: u32 = 0;
    let mut warnings: u32 = 0;
    for d in diags {
        match d.severity {
            1 => errors = errors.saturating_add(1),
            2 => warnings = warnings.saturating_add(1),
            _ => {}
        }
    }
    (errors, warnings)
}

/// Resolution of a file path to its language server and launch invocation.
struct Resolved {
    server: &'static origin_lspfleet::LspServer,
    program: &'static str,
    args: Vec<&'static str>,
    language_id: &'static str,
}

/// Resolve a file path to its language server + launch invocation via the fleet
/// registry.
///
/// Splits the registry `launch` string into a program (first token) and its
/// remaining args (for example `["--stdio"]`), and uses the server's language
/// name as the `did_open` `languageId`. Returns `None` when no registered
/// server claims the path's extension (or the path has no extension / an empty
/// launch string). Pure — performs no I/O.
fn resolve_launch(path: &Path) -> Option<Resolved> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    let server = origin_lspfleet::server_for_extension(ext)?;
    let mut tokens = server.launch.split_whitespace();
    let program = tokens.next()?;
    if program.is_empty() {
        return None;
    }
    let args: Vec<&'static str> = tokens.collect();
    Some(Resolved {
        server,
        program,
        args,
        language_id: server.language,
    })
}

/// Ensure the launch `program` is resolvable on the `PATH`, optionally
/// auto-installing.
///
/// Returns `true` when the program is already resolvable. When it is missing and
/// `ORIGIN_LSP_AUTO` is set, runs the registry `install` command through the
/// platform shell (the same opt-in side effect the `origin lsp ensure` CLI
/// performs) and re-probes. Without the auto flag a missing binary yields
/// `false` and the caller skips the file. All install failures are swallowed.
fn ensure_binary(program: &str, server: &origin_lspfleet::LspServer) -> bool {
    if which::which(program).is_ok() {
        return true;
    }
    if std::env::var_os("ORIGIN_LSP_AUTO").is_none() {
        return false;
    }
    tracing::info!(program, install = server.install, "auto-installing LSP server");
    let status = run_install(server.install);
    match status {
        Ok(s) if s.success() => which::which(program).is_ok(),
        Ok(s) => {
            tracing::warn!(program, %s, "LSP auto-install exited unsuccessfully");
            false
        }
        Err(e) => {
            tracing::warn!(program, error = %e, "LSP auto-install failed to start");
            false
        }
    }
}

/// Run a registry install command through the platform shell. The command
/// string originates from the builtin [`origin_lspfleet`] registry, never from
/// model/user input.
fn run_install(install_cmd: &str) -> std::io::Result<std::process::ExitStatus> {
    #[cfg(windows)]
    {
        std::process::Command::new("cmd").arg("/C").arg(install_cmd).status()
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("sh").arg("-c").arg(install_cmd).status()
    }
}

/// Probe one edited file: resolve its server, ensure the binary, spawn it, open
/// the file, and collect diagnostics for it under [`PER_FILE_TIMEOUT`].
///
/// Returns the diagnostics the server reported for `path` (possibly empty), or
/// an empty vec for every best-effort skip (no server, missing binary, unreadable
/// file, spawn/handshake failure). Never panics, never propagates an error.
async fn probe_file(workspace_root: &Path, path: &Path) -> Vec<Diagnostic> {
    let Some(resolved) = resolve_launch(path) else {
        return Vec::new();
    };
    if !ensure_binary(resolved.program, resolved.server) {
        tracing::debug!(program = resolved.program, "LSP server binary unavailable; skipping diagnostics");
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let program = resolved.program;
    match LspClient::diagnose_file(
        program,
        &resolved.args,
        workspace_root,
        path,
        resolved.language_id,
        &text,
        PER_FILE_TIMEOUT,
    )
    .await
    {
        Ok((_client, diags)) => {
            // `_client` is bound (not discarded with `_`) so the server stays
            // alive through the full collection window above; it is reaped here
            // when it drops at the end of this scope (kill_on_drop).
            diags
        }
        Err(e) => {
            tracing::debug!(program, error = %e, "LSP diagnostics probe failed (ignored)");
            Vec::new()
        }
    }
}

/// Build the `<lsp-diagnostics>` block for a turn's distinct edited files.
///
/// No-op (returns `None`) unless [`enabled`]. Probes up to [`MAX_FILES_PER_TURN`]
/// distinct paths (sorted for determinism), aggregates every server's reported
/// diagnostics, and formats them via [`format_diagnostics_block`]. A clean edit
/// (no diagnostics) and an unresolvable/unusable server set both yield `None`,
/// so the next turn's prompt stays unchanged. Best-effort throughout.
///
/// `workspace_root` is the directory the servers `initialize` against (the
/// session cwd). `edited` is the set of file paths the turn mutated.
#[allow(clippy::module_name_repetitions)] // `lsp_diagnostics_block` is the documented entry point callers expect.
pub async fn lsp_diagnostics_block(workspace_root: &Path, edited: &BTreeSet<String>) -> Option<String> {
    if !enabled() || edited.is_empty() {
        return None;
    }
    let mut all: Vec<Diagnostic> = Vec::new();
    for path_str in edited.iter().take(MAX_FILES_PER_TURN) {
        let path = Path::new(path_str);
        let mut diags = probe_file(workspace_root, path).await;
        all.append(&mut diags);
    }
    format_diagnostics_block(&all)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        collapse_ws, count_severities, enabled, format_diagnostics_block, render_one,
        resolve_launch, severity_label,
    };
    use origin_lsp_client::Diagnostic;
    use std::path::{Path, PathBuf};

    fn diag(file: &str, line: u32, col: u32, severity: u8, message: &str, code: Option<&str>) -> Diagnostic {
        Diagnostic {
            file: PathBuf::from(file),
            line,
            col,
            severity,
            message: message.to_owned(),
            code: code.map(str::to_owned),
        }
    }

    /// The gate is default-off: with the env var unset there is no block, so the
    /// next turn's system prompt is byte-identical. (Guarded so concurrent tests
    /// that set the var don't make this flaky; we assert the unset semantics.)
    #[test]
    fn gate_defaults_off() {
        // SAFETY of test ordering: we only read here. The production default is
        // "unset ⇒ false"; assert that explicitly via the documented contract.
        if std::env::var_os("ORIGIN_LSP_DIAGNOSTICS").is_none() {
            assert!(!enabled(), "feature must be off when the env var is unset");
        }
    }

    #[tokio::test]
    async fn block_is_none_when_disabled_even_with_edits() {
        // With the gate off, the async entry point must not spawn anything and
        // must return None regardless of the edited set.
        if std::env::var_os("ORIGIN_LSP_DIAGNOSTICS").is_none() {
            let mut edited = std::collections::BTreeSet::new();
            edited.insert("main.rs".to_string());
            let block = super::lsp_diagnostics_block(Path::new("."), &edited).await;
            assert!(block.is_none(), "disabled gate must yield no block");
        }
    }

    #[test]
    fn severity_labels_map_codes() {
        assert_eq!(severity_label(1), "error");
        assert_eq!(severity_label(2), "warning");
        assert_eq!(severity_label(3), "info");
        assert_eq!(severity_label(4), "hint");
        assert_eq!(severity_label(99), "warning"); // unknown → warning
    }

    #[test]
    fn render_one_is_one_based_and_includes_code() {
        // LSP line/col are zero-based; rendered output is 1-based.
        let d = diag("src/lib.rs", 9, 4, 1, "mismatched types", Some("E0308"));
        assert_eq!(render_one(&d), "src/lib.rs:10:5 error[E0308] mismatched types");
        let d2 = diag("a.rs", 0, 0, 2, "unused import", None);
        assert_eq!(render_one(&d2), "a.rs:1:1 warning unused import");
    }

    #[test]
    fn collapse_ws_flattens_multiline_messages() {
        assert_eq!(collapse_ws("a\n  b\tc  "), "a b c");
    }

    #[test]
    fn count_severities_counts_errors_and_warnings_only() {
        let diags = vec![
            diag("a.rs", 1, 1, 1, "e1", None),
            diag("a.rs", 2, 1, 1, "e2", None),
            diag("a.rs", 3, 1, 2, "w1", None),
            diag("a.rs", 4, 1, 3, "i1", None),
            diag("a.rs", 5, 1, 4, "h1", None),
        ];
        assert_eq!(count_severities(&diags), (2, 1));
    }

    #[test]
    fn format_block_none_when_empty() {
        assert!(format_diagnostics_block(&[]).is_none());
    }

    #[test]
    fn format_block_renders_sorted_deduped() {
        let diags = vec![
            diag("b.rs", 4, 0, 2, "later warning", None),
            diag("a.rs", 9, 4, 1, "boom", Some("E0308")),
            diag("a.rs", 9, 4, 1, "boom", Some("E0308")), // exact dup
        ];
        let block = format_diagnostics_block(&diags).unwrap();
        assert!(block.starts_with("<lsp-diagnostics>"));
        assert!(block.ends_with("</lsp-diagnostics>"));
        assert!(block.contains("summary: 1 error(s), 1 warning(s)"));
        // a.rs error sorts before b.rs warning; dup collapses to a single line.
        let a_idx = block.find("a.rs:10:5 error[E0308] boom").unwrap();
        let b_idx = block.find("b.rs:5:1 warning later warning").unwrap();
        assert!(a_idx < b_idx, "diagnostics must be sorted by path/line");
        assert_eq!(
            block.matches("a.rs:10:5 error[E0308] boom").count(),
            1,
            "duplicate diagnostic must be deduplicated"
        );
    }

    #[test]
    fn format_block_caps_lines_with_overflow_summary() {
        let total = super::MAX_DIAG_LINES + 5;
        let diags: Vec<Diagnostic> = (0..total)
            .map(|i| diag("a.rs", u32::try_from(i).unwrap_or(u32::MAX), 0, 2, &format!("w{i}"), None))
            .collect();
        let block = format_diagnostics_block(&diags).unwrap();
        assert!(block.contains("… and 5 more"));
    }

    #[test]
    fn resolve_launch_splits_program_and_args() {
        // pyright's launch is "pyright-langserver --stdio".
        let r = resolve_launch(Path::new("mod.py")).unwrap();
        assert_eq!(r.program, "pyright-langserver");
        assert_eq!(r.args, vec!["--stdio"]);
        assert_eq!(r.language_id, "python");
        assert_eq!(r.server.server_id, "pyright");
    }

    #[test]
    fn resolve_launch_bare_binary_has_no_args() {
        // rust-analyzer's launch is just "rust-analyzer".
        let r = resolve_launch(Path::new("src/main.rs")).unwrap();
        assert_eq!(r.program, "rust-analyzer");
        assert!(r.args.is_empty());
        assert_eq!(r.language_id, "rust");
    }

    #[test]
    fn resolve_launch_unknown_extension_is_none() {
        assert!(resolve_launch(Path::new("notes.unknownext")).is_none());
        assert!(resolve_launch(Path::new("noext")).is_none());
    }
}
