// SPDX-License-Identifier: Apache-2.0
//! `origin plugin ls` / `plugin info` — plugin discovery and inspection.
//!
//! `ls` discovers live `.claude` / `.agents` skills under the current directory
//! and the home directory (kilocode claudeCodeCompat, opencode live `.claude`
//! reading). `info` parses a plugin manifest and reports its declared surface
//! plus a context-window cost estimate (claude-code marketplace context-cost).
//! All parsing / discovery logic lives in the pure [`origin_plugin`] crate.

use anyhow::Result;
use origin_plugin::{context_cost_estimate, discover_skills, parse_manifest};

use crate::cli_def::PluginSub;

/// Dispatch a `plugin` subcommand.
///
/// # Errors
/// Returns on filesystem or manifest-parse failure.
pub fn run(sub: PluginSub) -> Result<()> {
    match sub {
        PluginSub::Ls => ls(),
        PluginSub::Info { manifest } => info(&manifest),
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
