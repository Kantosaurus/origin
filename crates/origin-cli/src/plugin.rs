// SPDX-License-Identifier: Apache-2.0
//! `origin plugin ls` / `plugin info` / `plugin install` — plugin discovery,
//! inspection, and local install.
//!
//! `ls` discovers live `.claude` / `.agents` skills under the current directory
//! and the home directory (kilocode claudeCodeCompat, opencode live `.claude`
//! reading). `info` parses a plugin manifest and reports its declared surface
//! plus a context-window cost estimate (claude-code marketplace context-cost).
//! `install` places a plugin bundle (from a local path or git URL) under
//! `~/.origin/plugins/<name>/`, validates its manifest, resolves its declared
//! dependency order, and prints the installed surface + context cost (the local
//! half of the claude-code plugin marketplace and gemini `extensions install`).
//! All parsing / discovery / install logic lives in the pure [`origin_plugin`]
//! crate; the git clone reuses the same `std::process` approach as `scout`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use origin_plugin::{
    context_cost_estimate, discover_skills, install_into, parse_manifest, resolve_order, Manifest,
};

use crate::cli_def::PluginSub;

/// Dispatch a `plugin` subcommand.
///
/// # Errors
/// Returns on filesystem or manifest-parse failure.
pub fn run(sub: PluginSub) -> Result<()> {
    match sub {
        PluginSub::Ls => ls(),
        PluginSub::Info { manifest } => info(&manifest),
        PluginSub::Install { source } => install(&source),
    }
}

/// List discovered skills across the current directory and the home directory.
fn ls() -> Result<()> {
    let mut roots: Vec<String> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd.to_string_lossy().into_owned());
    }
    if let Some(home) = dirs::home_dir() {
        roots.push(home.to_string_lossy().into_owned());
    }
    let skills = discover_skills(&roots).map_err(|e| anyhow::anyhow!("discovering skills: {e}"))?;
    if skills.is_empty() {
        println!("no skills discovered");
        return Ok(());
    }
    for s in skills {
        println!("{}  {}  {}", s.source, s.name, s.path);
    }
    Ok(())
}

/// Parse and describe a single plugin manifest.
fn info(manifest_path: &str) -> Result<()> {
    let src = std::fs::read_to_string(manifest_path)
        .map_err(|e| anyhow::anyhow!("reading {manifest_path}: {e}"))?;
    let manifest = parse_manifest(&src).map_err(|e| anyhow::anyhow!("parsing manifest: {e}"))?;
    let cost = context_cost_estimate(&manifest);
    println!("name:    {}", manifest.name);
    println!("version: {}", manifest.version);
    println!("context cost estimate: ~{cost} tokens");
    Ok(())
}

/// Returns the plugins root: `~/.origin/plugins`.
fn plugins_root() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    Ok(home.join(".origin").join("plugins"))
}

/// Heuristically decides whether `source` looks like a git URL rather than a
/// local path. Recognises the same schemes `scout` accepts plus `git@` SSH.
fn looks_like_git_url(source: &str) -> bool {
    source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("git://")
        || source.starts_with("ssh://")
        || source.starts_with("git@")
}

/// Shallow-clones `url` into `dest` by shelling out to the system `git` binary.
///
/// This mirrors `scout`'s clone (a `--depth 1 --filter=blob:none` clone via
/// `std::process::Command`) so no new dependency is introduced.
fn shallow_clone(url: &str, dest: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["clone", "--depth", "1", "--filter=blob:none", url])
        .arg(dest)
        .output()
        .map_err(|e| anyhow::anyhow!("spawning git: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(anyhow::anyhow!("git clone failed: {stderr}"))
    }
}

/// Install a plugin bundle from a local path or git URL into the plugins root.
///
/// A local directory is copied directly; a git URL is first shallow-cloned into
/// a staging directory (removed afterwards) and then copied into place. In both
/// cases the manifest is validated and the install destination is named after
/// the manifest's `name`. On manifest-invalid input the partially-placed plugin
/// directory is removed and a clear error is returned (no panic). Re-installing
/// the same plugin overwrites cleanly (idempotent).
///
/// # Errors
/// Returns on filesystem failure, a failed `git clone`, or an invalid manifest.
pub fn install(source: &str) -> Result<()> {
    let root = plugins_root()?;
    std::fs::create_dir_all(&root)
        .map_err(|e| anyhow::anyhow!("creating {}: {e}", root.display()))?;

    if looks_like_git_url(source) {
        install_from_git(source, &root)
    } else {
        let src = PathBuf::from(source);
        if !src.is_dir() {
            return Err(anyhow::anyhow!(
                "source `{source}` is not an existing directory and is not a recognised git URL"
            ));
        }
        let (manifest, dest) = install_into(&src, &root)
            .map_err(|e| anyhow::anyhow!("installing plugin: {e}"))?;
        report(&manifest, &dest);
        Ok(())
    }
}

/// Clones `source` into a staging dir, installs it, and removes the staging dir.
fn install_from_git(source: &str, root: &Path) -> Result<()> {
    // Stage the clone under the plugins root so the eventual copy is same-volume
    // and the staging area is easy to find/clean up.
    let staging = root.join(format!(".staging-{}", staging_suffix(source)));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|e| anyhow::anyhow!("clearing staging {}: {e}", staging.display()))?;
    }
    println!("cloning {source} …");
    if let Err(e) = shallow_clone(source, &staging) {
        std::fs::remove_dir_all(&staging).ok();
        return Err(e);
    }

    let installed = install_into(&staging, root)
        .map_err(|e| anyhow::anyhow!("installing plugin: {e}"));
    // Always remove the staging clone, success or failure.
    std::fs::remove_dir_all(&staging).ok();
    let (manifest, dest) = installed?;
    report(&manifest, &dest);
    Ok(())
}

/// A short, filesystem-safe suffix derived from `source` for the staging dir.
fn staging_suffix(source: &str) -> String {
    source
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(24)
        .collect()
}

/// Prints the installed surface (per-kind counts), the dependency install order,
/// and the context-cost estimate for a freshly installed `manifest`.
fn report(manifest: &Manifest, dest: &Path) {
    let cost = context_cost_estimate(manifest);
    println!("installed `{}` v{} -> {}", manifest.name, manifest.version, dest.display());
    println!(
        "surface: {} commands, {} agents, {} skills, {} hooks, {} mcp, {} lsp",
        manifest.commands.len(),
        manifest.agents.len(),
        manifest.skills.len(),
        manifest.hooks.len(),
        manifest.mcp.len(),
        manifest.lsp.len(),
    );
    // The plugin depends only on itself here, but resolving over the single
    // manifest validates its declared deps shape and surfaces a clean order line
    // (full cross-plugin resolution happens once a marketplace index exists).
    match resolve_order(std::slice::from_ref(manifest)) {
        Ok(order) => {
            if !order.is_empty() {
                println!("install order: {}", order.join(" -> "));
            }
        }
        Err(e) => println!("dependency note: {e}"),
    }
    println!("context cost estimate: ~{cost} tokens");
}
