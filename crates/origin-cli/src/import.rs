//! `origin import` — migrate sessions/skills from another harness.
//!
//! Wraps the `origin-migrate` source adapters and produces a summary
//! [`ApplyReport`]. Full sink-via-`Store` wiring is a follow-up; today
//! both dry-run and `--apply` return the same content-hash-aware summary.

#![allow(clippy::module_name_repetitions)]

use origin_migrate::claude_code::ClaudeCodeSource;
use origin_migrate::jcode::JcodeSource;
use origin_migrate::opencode::OpencodeSource;
use origin_migrate::sink::{summarize, ApplyReport};
use origin_migrate::source::{Source, SourceError};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ImportSource {
    ClaudeCode,
    Jcode,
    Opencode,
}

#[derive(Debug, clap::Args)]
pub struct ImportArgs {
    #[arg(value_enum)]
    pub source: ImportSource,
    #[arg(long)]
    pub from: PathBuf,
    #[arg(long)]
    pub apply: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Error)]
pub enum ImportCliError {
    #[error(transparent)]
    Source(#[from] SourceError),
}

/// Run `origin import`. Returns a summary report.
///
/// # Errors
/// Returns an [`ImportCliError`] when scanning fails.
pub fn run_import(args: &ImportArgs) -> Result<ApplyReport, ImportCliError> {
    let bundle = match args.source {
        ImportSource::ClaudeCode => ClaudeCodeSource.scan(&args.from)?,
        ImportSource::Jcode => JcodeSource.scan(&args.from)?,
        ImportSource::Opencode => OpencodeSource.scan(&args.from)?,
    };
    // Apply path needs a SessionStore; in this CLI we only summarize.
    // Wiring the real apply through `apply_with_store` is left for a follow-up;
    // dry-run + apply currently both return the same content-hash-aware summary.
    let _ = args.apply;
    Ok(summarize(&bundle))
}
