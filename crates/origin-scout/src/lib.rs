// SPDX-License-Identifier: Apache-2.0
//! Read-only dependency-source research helpers.
//!
//! Plans a shallow clone of a dependency repository into a managed cache and
//! extracts a compact overview (README excerpt, manifest summary, top-level
//! directories, and likely entry points). Git access is injected through the
//! [`CloneRunner`] trait so the crate is fully unit-testable offline, while the
//! overview extraction in [`build_overview`] is pure and side-effect free.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors raised while planning a clone or summarizing a repository.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ScoutError {
    /// The injected git runner reported a failure.
    #[error("git error: {0}")]
    Git(String),
    /// A filesystem operation failed.
    #[error("io error: {0}")]
    Io(String),
}

/// Performs the actual (side-effecting) git clone.
///
/// Implementors run a real `git clone --depth 1`; tests supply a mock so no
/// network or process is required.
pub trait CloneRunner {
    /// Shallow-clones `url` into `dest`.
    ///
    /// # Errors
    ///
    /// Returns [`ScoutError::Git`] if the clone fails, or [`ScoutError::Io`]
    /// if the destination cannot be prepared.
    fn shallow_clone(&self, url: &str, dest: &str) -> Result<(), ScoutError>;
}

/// Maximum number of bytes retained from a README excerpt.
const README_EXCERPT_LIMIT: usize = 800;

/// Computes a deterministic 64-bit FNV-1a hash of `bytes`.
///
/// A hand-rolled hash keeps the crate dependency-light while still producing a
/// stable, url-derived cache directory name across runs and platforms.
const fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

/// Returns a deterministic cache directory path for `repo_url` under `cache_root`.
///
/// The directory name is derived from a hash of the url, so the same url always
/// maps to the same location and distinct urls map to distinct locations.
#[must_use]
pub fn cache_path(cache_root: &str, repo_url: &str) -> String {
    let hash = fnv1a_64(repo_url.as_bytes());
    let trimmed = cache_root.trim_end_matches(['/', '\\']);
    format!("{trimmed}/scout-{hash:016x}")
}

/// Plans a shallow clone of `repo_url` under `cache_root`.
///
/// Returns the deterministic destination directory and a cached-hint flag. The
/// hint is always `false` here: this function performs no filesystem access, so
/// the caller is responsible for checking whether the destination already
/// exists before invoking a [`CloneRunner`].
#[must_use]
pub fn clone_plan(cache_root: &str, repo_url: &str) -> (String, bool) {
    (cache_path(cache_root, repo_url), false)
}

/// A compact, human-readable summary of a cloned repository.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overview {
    /// Leading excerpt of the README, truncated to a bounded length.
    pub readme_excerpt: String,
    /// One-line summary identifying the detected package manifest.
    pub manifest_summary: String,
    /// Sorted, de-duplicated set of top-level directory names.
    pub top_dirs: Vec<String>,
    /// Detected likely program entry points.
    pub entry_points: Vec<String>,
}

/// Normalizes a path listing entry to forward slashes.
fn normalize(path: &str) -> String {
    path.replace('\\', "/")
}

/// Returns the first path component of `path`, if it has a nested component.
fn top_dir(path: &str) -> Option<String> {
    let norm = normalize(path);
    let trimmed = norm.trim_start_matches('/');
    let (head, rest) = trimmed.split_once('/')?;
    if head.is_empty() || rest.is_empty() {
        return None;
    }
    Some(head.to_owned())
}

/// Returns `true` when `path` looks like a conventional program entry point.
fn is_entry_point(path: &str) -> bool {
    const ENTRIES: [&str; 8] = [
        "src/main.rs",
        "src/lib.rs",
        "src/index.ts",
        "src/index.js",
        "index.ts",
        "index.js",
        "main.py",
        "src/main.py",
    ];
    let norm = normalize(path);
    let lower = norm.to_ascii_lowercase();
    ENTRIES.iter().any(|e| lower == *e)
}

/// Builds a one-line manifest summary from a manifest file's content.
fn summarize_manifest(file_list: &[String], manifest: Option<&str>) -> String {
    let kind = file_list.iter().find_map(|f| {
        let lower = normalize(f).to_ascii_lowercase();
        match lower.as_str() {
            "cargo.toml" => Some("Rust (Cargo.toml)"),
            "package.json" => Some("Node.js (package.json)"),
            "pyproject.toml" => Some("Python (pyproject.toml)"),
            _ => None,
        }
    });
    kind.map_or_else(
        || "no recognized manifest".to_owned(),
        |label| {
            let bytes = manifest.map_or(0, str::len);
            format!("{label}, {bytes} bytes")
        },
    )
}

/// Truncates `readme` to a bounded excerpt on a UTF-8 boundary.
fn excerpt(readme: Option<&str>) -> String {
    let Some(text) = readme else {
        return String::new();
    };
    let trimmed = text.trim_start();
    if trimmed.len() <= README_EXCERPT_LIMIT {
        return trimmed.to_owned();
    }
    let mut end = README_EXCERPT_LIMIT;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = trimmed[..end].to_owned();
    out.push('\u{2026}');
    out
}

/// Summarizes a cloned repository from a file listing plus key file contents.
///
/// `file_list` is a flat listing of repository-relative paths. `readme` and
/// `manifest` are the contents of the README and the detected package manifest,
/// if available. Entry points (e.g. `src/main.rs`, `index.ts`) and top-level
/// directories are inferred from `file_list`; all results are sorted and
/// de-duplicated for stable output.
#[must_use]
pub fn build_overview(file_list: &[String], readme: Option<&str>, manifest: Option<&str>) -> Overview {
    let mut top_dirs: Vec<String> = file_list.iter().filter_map(|p| top_dir(p)).collect();
    top_dirs.sort_unstable();
    top_dirs.dedup();

    let mut entry_points: Vec<String> = file_list
        .iter()
        .filter(|p| is_entry_point(p))
        .map(|p| normalize(p))
        .collect();
    entry_points.sort_unstable();
    entry_points.dedup();

    Overview {
        readme_excerpt: excerpt(readme),
        manifest_summary: summarize_manifest(file_list, manifest),
        top_dirs,
        entry_points,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_is_stable_and_url_derived() {
        let a = cache_path("/cache", "https://github.com/a/b.git");
        let b = cache_path("/cache", "https://github.com/a/b.git");
        assert_eq!(a, b, "same url must yield same path");
        let c = cache_path("/cache", "https://github.com/x/y.git");
        assert_ne!(a, c, "different urls must differ");
        assert!(a.starts_with("/cache/scout-"));
    }

    #[test]
    fn cache_path_trims_trailing_separators() {
        let a = cache_path("/cache/", "u");
        let b = cache_path("/cache", "u");
        assert_eq!(a, b);
    }

    #[test]
    fn clone_plan_reports_dest_without_cached_hint() {
        let (dest, cached) = clone_plan("/cache", "https://example.com/r.git");
        assert_eq!(dest, cache_path("/cache", "https://example.com/r.git"));
        assert!(!cached);
    }

    #[test]
    fn build_overview_detects_entry_points() {
        let files = vec![
            "src/main.rs".to_owned(),
            "src/index.ts".to_owned(),
            "README.md".to_owned(),
        ];
        let ov = build_overview(&files, None, None);
        assert_eq!(ov.entry_points, vec!["src/index.ts", "src/main.rs"]);
    }

    #[test]
    fn build_overview_detects_top_dirs() {
        let files = vec![
            "src/main.rs".to_owned(),
            "tests/it.rs".to_owned(),
            "src/util.rs".to_owned(),
            "README.md".to_owned(),
        ];
        let ov = build_overview(&files, None, None);
        assert_eq!(ov.top_dirs, vec!["src", "tests"]);
    }

    #[test]
    fn manifest_summary_distinguishes_cargo_and_package_json() {
        let cargo = vec!["Cargo.toml".to_owned()];
        let node = vec!["package.json".to_owned()];
        let py = vec!["pyproject.toml".to_owned()];
        assert!(build_overview(&cargo, None, Some("x")).manifest_summary.starts_with("Rust"));
        assert!(build_overview(&node, None, Some("xx")).manifest_summary.starts_with("Node.js"));
        assert!(build_overview(&py, None, None).manifest_summary.starts_with("Python"));
        assert_eq!(
            build_overview(&[], None, None).manifest_summary,
            "no recognized manifest"
        );
    }

    #[test]
    fn readme_excerpt_truncates_long_input() {
        let long = "a".repeat(README_EXCERPT_LIMIT + 50);
        let ov = build_overview(&[], Some(&long), None);
        assert!(ov.readme_excerpt.ends_with('\u{2026}'));
        assert!(ov.readme_excerpt.chars().count() <= README_EXCERPT_LIMIT + 1);
    }

    #[test]
    fn readme_excerpt_trims_leading_whitespace_only() {
        // `excerpt` trims leading whitespace but preserves trailing content.
        let ov = build_overview(&[], Some("  hello world\n"), None);
        assert_eq!(ov.readme_excerpt, "hello world\n");
    }

    #[test]
    fn empty_inputs_are_handled() {
        let ov = build_overview(&[], None, None);
        assert_eq!(ov.readme_excerpt, "");
        assert_eq!(ov.manifest_summary, "no recognized manifest");
        assert!(ov.top_dirs.is_empty());
        assert!(ov.entry_points.is_empty());
    }

    #[test]
    fn overview_is_serde_roundtrippable() {
        let files = vec!["src/main.rs".to_owned(), "Cargo.toml".to_owned()];
        let ov = build_overview(&files, Some("readme"), Some("[package]"));
        let json = serde_json::to_string(&ov).unwrap();
        let back: Overview = serde_json::from_str(&json).unwrap();
        assert_eq!(ov, back);
    }

    #[test]
    fn clone_runner_mock_is_invokable() {
        struct Mock;
        impl CloneRunner for Mock {
            fn shallow_clone(&self, url: &str, _dest: &str) -> Result<(), ScoutError> {
                if url.is_empty() {
                    return Err(ScoutError::Git("empty url".to_owned()));
                }
                Ok(())
            }
        }
        let m = Mock;
        assert!(m.shallow_clone("https://x/y.git", "/tmp/d").is_ok());
        assert_eq!(
            m.shallow_clone("", "/tmp/d").unwrap_err(),
            ScoutError::Git("empty url".to_owned())
        );
    }
}
