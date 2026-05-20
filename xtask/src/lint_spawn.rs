//! Lint: ban `tokio::spawn` / `tokio::task::spawn` / `tokio::task::spawn_blocking`
//! outside the sanctioned `origin-runtime::spawn_in` site (+ allowlist).

#![allow(
    clippy::module_name_repetitions,
    clippy::missing_errors_doc,
    clippy::needless_pass_by_value
)]

use std::path::{Path, PathBuf};

use clap::Args as ClapArgs;
use walkdir::WalkDir;

use crate::lint_spawn_allowlist::is_allowlisted;

const BANNED_PATTERNS: &[&str] = &[
    "tokio::spawn(",
    "tokio::task::spawn(",
    "tokio::task::spawn_blocking(",
];

/// Arguments for the `lint-spawn` subcommand.
#[derive(Debug, ClapArgs)]
pub struct CliArgs {
    /// Path to scan. Defaults to the workspace root.
    #[arg(long, default_value = ".")]
    pub root: PathBuf,
}

pub use CliArgs as Args;

/// Run the lint. Returns the process exit code: `0` clean, `1` on violation.
#[must_use]
pub fn run(args: Args) -> i32 {
    match scan(&args.root) {
        Ok(()) => 0,
        Err(msg) => {
            eprintln!("lint-spawn: {msg}");
            1
        }
    }
}

fn is_crate_subdir(path: &str, kind: &str) -> bool {
    // True when `path` lies under `*/<kind>/...` BUT NOT under any `fixtures/`
    // subtree (lint fixtures intentionally contain raw spawn).
    let needle = format!("/{kind}/");
    if !path.contains(needle.as_str()) && !path.starts_with(&format!("{kind}/")) {
        return false;
    }
    // Reject anything inside a fixtures tree — that's lint input, not real tests.
    if path.contains("/fixtures/") {
        return false;
    }
    true
}

fn scan(root: &Path) -> Result<(), String> {
    let mut violations: Vec<(String, usize, String)> = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("rs"))
    {
        let rel = entry.path().display().to_string();
        // Integration tests / benches / build scripts / target are exempt.
        // Match real crate test/bench dirs (`crates/<name>/tests/`), not
        // arbitrary `tests` substrings (so xtask fixture trees still scan).
        let normalized = rel.replace('\\', "/");
        if is_crate_subdir(&normalized, "tests")
            || is_crate_subdir(&normalized, "benches")
            || normalized.contains("/target/")
            || normalized.ends_with("build.rs")
        {
            continue;
        }
        if is_allowlisted(&rel) {
            continue;
        }
        let src = std::fs::read_to_string(entry.path()).map_err(|e| format!("read {rel}: {e}"))?;
        for (lineno, line) in src.lines().enumerate() {
            // Skip lines inside a string literal or comment in the cheapest
            // way that still catches the common cases.
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            for pat in BANNED_PATTERNS {
                if line.contains(pat) {
                    violations.push((rel.clone(), lineno + 1, line.trim().to_string()));
                }
            }
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    for (path, line, snippet) in &violations {
        eprintln!("error: raw spawn at {path}:{line}: {snippet}");
    }
    Err(format!("{} violation(s)", violations.len()))
}
