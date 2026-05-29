// SPDX-License-Identifier: Apache-2.0
//! P11.11 — `origin trace query` subcommand.
//!
//! Wraps `origin_trace::query::run` with a clap-derived arg struct and a
//! pretty-printer over the row stream. The default trace directory mirrors
//! `origin_daemon::main`'s default (`$XDG_DATA_HOME/origin/trace`).

use std::path::PathBuf;

use clap::Args;
use origin_trace::query::{run, QueryArgs};

#[derive(Debug, Args)]
pub struct TraceQuery {
    /// Trace ring directory. Defaults to `$XDG_DATA_HOME/origin/trace`
    /// (or the platform local-data dir on Windows / macOS).
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Filter by `kind` column (e.g. `tool`, `provider`, `turn`).
    #[arg(long)]
    pub kind: Option<String>,
    /// Filter by `error_kind` column (e.g. `Sandbox`, `Provider429`).
    #[arg(long)]
    pub error_kind: Option<String>,
    /// Maximum rows to print.
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
}

/// Invoke the trace query and pretty-print rows to stdout.
///
/// # Errors
/// Returns [`origin_trace::query::QueryError`] on parquet/io failure.
pub fn invoke(args: TraceQuery) -> Result<(), Box<dyn std::error::Error>> {
    let dir = args.dir.unwrap_or_else(|| {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("origin")
            .join("trace")
    });
    let q = QueryArgs {
        dir,
        kind: args.kind,
        error_kind: args.error_kind,
        limit: args.limit,
    };
    let rows = run(&q)?;
    for row in rows {
        let error_kind = if row.error_kind.is_empty() {
            "-".to_string()
        } else {
            row.error_kind
        };
        println!(
            "{ts_ns:>20} {kind:<10} {provider:<12} {tool:<16} dur={dur_us}µs err={error_kind} attrs={attrs_json}",
            ts_ns = row.ts_ns,
            kind = row.kind,
            provider = row.provider,
            tool = row.tool,
            dur_us = row.dur_us,
            error_kind = error_kind,
            attrs_json = row.attrs_json,
        );
    }
    Ok(())
}
