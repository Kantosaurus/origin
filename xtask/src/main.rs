//! `xtask` — workspace developer-tools binary.
//!
//! Currently hosts the `lint-secrets` subcommand which enforces the
//! `Secret<T>` redaction convention across the `origin` workspace.

use clap::{Parser, Subcommand};

mod lint_secrets;

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
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::LintSecrets(a) => lint_secrets::run(a),
    };
    std::process::exit(code);
}
