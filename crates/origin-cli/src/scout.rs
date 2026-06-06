// SPDX-License-Identifier: Apache-2.0
//! `origin scout <repo_url>` — read-only dependency-source research.
//!
//! Plans a shallow clone of a dependency repository into a managed cache
//! (`~/.origin/scout`), shells `git clone --depth 1 --filter=blob:none`, walks
//! the result, and prints a compact overview (README excerpt, manifest summary,
//! top-level dirs, entry points) computed by the pure [`origin_scout`] crate
//! (opencode scout / `repo_overview` parity).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use origin_scout::{build_overview, clone_plan, CloneRunner, ScoutError};

/// A [`CloneRunner`] that shells out to the system `git` binary.
struct CmdClone;

impl CloneRunner for CmdClone {
    fn shallow_clone(&self, url: &str, dest: &str) -> Result<(), ScoutError> {
        let output = Command::new("git")
            .args(["clone", "--depth", "1", "--filter=blob:none", url, dest])
            .output()
            .map_err(|e| ScoutError::Git(format!("spawning git: {e}")))?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            Err(ScoutError::Git(stderr))
        }
    }
}

/// Default cache root: `~/.origin/scout`.
fn default_cache_root() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    Ok(home.join(".origin").join("scout"))
}

/// Recursively collects repository-relative file paths under `root`.
///
/// `.git` is skipped. Failures on individual entries are tolerated so a single
/// unreadable directory does not abort the walk.
fn collect_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

/// Depth-first helper for [`collect_files`].
fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if entry.file_name() == ".git" {
                continue;
            }
            walk(root, &path, out);
        } else if file_type.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
}

/// Reads the first matching file (by repo-relative name) under `dest`, if any.
fn read_first(dest: &Path, files: &[String], names: &[&str]) -> Option<String> {
    for rel in files {
        let base = rel.rsplit('/').next().unwrap_or(rel.as_str());
        if names.iter().any(|n| n.eq_ignore_ascii_case(base)) {
            if let Ok(content) = std::fs::read_to_string(dest.join(rel)) {
                return Some(content);
            }
        }
    }
    None
}

/// Run `origin scout`: clone (if needed), summarize, and print the overview.
///
/// # Errors
/// Returns on filesystem failure or when the underlying `git clone` fails.
pub fn run(repo_url: &str, cache: Option<String>) -> Result<()> {
    let cache_root = match cache {
        Some(c) => PathBuf::from(c),
        None => default_cache_root()?,
    };
    std::fs::create_dir_all(&cache_root)
        .map_err(|e| anyhow::anyhow!("creating {}: {e}", cache_root.display()))?;

    let (dest, _cached) = clone_plan(&cache_root.to_string_lossy(), repo_url);
    let dest_path = PathBuf::from(&dest);

    if dest_path.exists() {
        println!("using cached clone at {dest}");
    } else {
        println!("cloning {repo_url} into {dest} …");
        CmdClone
            .shallow_clone(repo_url, &dest)
            .map_err(|e| anyhow::anyhow!("cloning repository: {e}"))?;
    }

    let files = collect_files(&dest_path);
    let readme = read_first(
        &dest_path,
        &files,
        &["README.md", "README.rst", "README.txt", "README"],
    );
    let manifest = read_first(
        &dest_path,
        &files,
        &["Cargo.toml", "package.json", "pyproject.toml"],
    );

    let overview = build_overview(&files, readme.as_deref(), manifest.as_deref());

    println!("\nmanifest: {}", overview.manifest_summary);
    if !overview.top_dirs.is_empty() {
        println!("top dirs: {}", overview.top_dirs.join(", "));
    }
    if !overview.entry_points.is_empty() {
        println!("entry points: {}", overview.entry_points.join(", "));
    }
    if !overview.readme_excerpt.is_empty() {
        println!("\n--- README excerpt ---\n{}", overview.readme_excerpt);
    }
    Ok(())
}
