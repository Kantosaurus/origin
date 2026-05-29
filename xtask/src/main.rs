// SPDX-License-Identifier: Apache-2.0
//! `xtask` — workspace developer-tools binary.
//!
//! Hosts the `lint-secrets` subcommand (enforces the `Secret<T>` redaction
//! convention), the `lint-spawn` subcommand (bans raw `tokio::spawn`
//! outside `origin-runtime::spawn_in`), and `manpages` which renders
//! `clap_mangen` output for the `origin` CLI (P14.D.4).

use clap::{Parser, Subcommand};

mod lint_secrets;
mod lint_spawn;
mod lint_spawn_allowlist;
mod manpages;
mod release;

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan Rust source for unwrapped secret-named fields under
    /// `#[derive(Debug)]`.
    LintSecrets(lint_secrets::Args),
    /// Scan Rust source for raw `tokio::spawn` outside the allowlist.
    LintSpawn(lint_spawn::Args),
    /// Render manpages for `origin` and every subcommand into `--out`.
    Manpages {
        /// Output directory. Created if missing.
        #[arg(long, default_value = "target/manpages")]
        out: std::path::PathBuf,
    },
    /// Stamp packaging templates with version + per-target SHA256s.
    Release {
        #[arg(long)]
        version: String,
        #[arg(long)]
        manifest: std::path::PathBuf,
        #[arg(long, default_value = "target/release-packaging")]
        out: std::path::PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::LintSecrets(a) => lint_secrets::run(a),
        Cmd::LintSpawn(a) => lint_spawn::run(a),
        Cmd::Manpages { out } => match manpages::generate(&out) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("manpages: {e}");
                1
            }
        },
        Cmd::Release {
            version,
            manifest,
            out,
        } => match release::stamp(&version, &manifest, &out) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("release: {e}");
                1
            }
        },
    };
    std::process::exit(code);
}
