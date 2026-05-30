// SPDX-License-Identifier: Apache-2.0
//! `origin watch` — editor-agnostic scan for AI-trigger comments.
//!
//! Scans a source tree for inline markers such as `// AI: ...`, `# AI! ...`, or
//! `-- AI? ...` and prints them as actionable items (aider `--watch-files`
//! parity), without depending on any editor. The scan logic lives in the pure
//! [`origin_watch`] crate.

use anyhow::Result;
use origin_watch::{scan_dir, AiKind, ScanConfig};

/// Default file extensions scanned when `--ext` is not supplied.
const DEFAULT_EXTS: [&str; 13] = [
    "rs", "py", "ts", "tsx", "js", "jsx", "go", "rb", "sh", "sql", "c", "cpp", "h",
];

/// Run `origin watch`: scan `root` (default: cwd) for AI-trigger comments.
///
/// `ext` is an optional comma-separated extension list; when absent the
/// [`DEFAULT_EXTS`] set is used.
///
/// # Errors
/// Returns on filesystem failure while walking the tree.
pub fn run(root: Option<String>, ext: Option<String>) -> Result<()> {
    let root = match root {
        Some(r) => r,
        None => std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("resolving cwd: {e}"))?
            .to_string_lossy()
            .into_owned(),
    };
    let extensions: Vec<String> = ext.map_or_else(
        || DEFAULT_EXTS.iter().map(|s| (*s).to_owned()).collect(),
        |csv| {
            csv.split(',')
                .map(|s| s.trim().trim_start_matches('.').to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        },
    );

    let cfg = ScanConfig { root, extensions };
    let comments = scan_dir(&cfg).map_err(|e| anyhow::anyhow!("scanning tree: {e}"))?;
    if comments.is_empty() {
        println!("no AI-trigger comments found");
        return Ok(());
    }
    for c in comments {
        let tag = match c.kind {
            AiKind::Ai => "AI",
            AiKind::Bang => "AI!",
            AiKind::Question => "AI?",
        };
        println!("{}:{} [{tag}] {}", c.file, c.line, c.text);
    }
    Ok(())
}
