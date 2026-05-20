//! `xtask` — workspace developer-tools binary.
//!
//! Hosts the `lint-secrets` subcommand (enforces the `Secret<T>` redaction
//! convention) and the `lint-spawn` subcommand (bans raw `tokio::spawn`
//! outside `origin-runtime::spawn_in`).

use clap::{Parser, Subcommand};

mod lint_secrets;
mod lint_spawn;
mod lint_spawn_allowlist;

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
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::LintSecrets(a) => lint_secrets::run(a),
        Cmd::LintSpawn(a) => lint_spawn::run(a),
    };
    std::process::exit(code);
}
