//! `origin import` — migrate sessions/skills from another harness.
//!
//! Wraps the `origin-migrate` source adapters. When `--apply` is set the
//! bundle is persisted through [`origin_migrate::sink::apply_with_store`]
//! against the same `SQLite` store the daemon uses (`ORIGIN_DB` env var, or
//! the platform-default temp path). Dry-run mode still returns the pure
//! [`summarize`](origin_migrate::sink::summarize) report.

#![allow(clippy::module_name_repetitions)]

use origin_migrate::claude_code::ClaudeCodeSource;
use origin_migrate::jcode::JcodeSource;
use origin_migrate::opencode::OpencodeSource;
use origin_migrate::sink::{apply_with_store, summarize, ApplyReport};
use origin_migrate::source::{Source, SourceError};
use origin_store::{Store, StoreError};
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
    /// Override the `SQLite` store path. Defaults to `ORIGIN_DB` or the
    /// temp-dir fallback used by the daemon.
    #[arg(long)]
    pub db: Option<PathBuf>,
}

#[derive(Debug, Error)]
pub enum ImportCliError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error("store open: {0}")]
    StoreOpen(String),
    #[error("store apply: {0}")]
    StoreApply(String),
}

impl From<StoreError> for ImportCliError {
    fn from(value: StoreError) -> Self {
        Self::StoreOpen(value.to_string())
    }
}

/// Run `origin import`. Returns a summary report.
///
/// # Errors
/// Returns an [`ImportCliError`] when scanning, store-open, or persistence
/// fails.
pub fn run_import(args: &ImportArgs) -> Result<ApplyReport, ImportCliError> {
    let bundle = match args.source {
        ImportSource::ClaudeCode => ClaudeCodeSource.scan(&args.from)?,
        ImportSource::Jcode => JcodeSource.scan(&args.from)?,
        ImportSource::Opencode => OpencodeSource.scan(&args.from)?,
    };

    if !args.apply {
        return Ok(summarize(&bundle));
    }

    let db_path = args
        .db
        .clone()
        .unwrap_or_else(|| PathBuf::from(default_db_path()));
    let store = Store::open(&db_path)?;
    apply_with_store(&store, &bundle).map_err(|e| ImportCliError::StoreApply(e.to_string()))
}

/// Resolve the daemon's `SQLite` path: `ORIGIN_DB` env var, falling back to
/// `<temp>/origin.db` (matches `origin-daemon::main::default_db_path`).
fn default_db_path() -> String {
    if let Ok(p) = std::env::var("ORIGIN_DB") {
        return p;
    }
    let mut p = std::env::temp_dir();
    p.push("origin.db");
    p.to_string_lossy().into_owned()
}
