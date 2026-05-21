//! Clap command definitions for the `origin` binary.
//!
//! These live in the library so that introspection tools (notably
//! `xtask manpages`, which renders `clap_mangen` output) can build the
//! same `clap::Command` tree without depending on the binary crate.

use crate::trace_cmd::TraceQuery;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "origin", version, about = "origin agentic coding harness")]
pub struct Cli {
    /// Run the 7-step interactive guided tour (P14.D.3).
    #[arg(long)]
    pub tutorial: bool,
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Query the trace ring (P11.11). Without any flags, prints the most
    /// recent 100 spans across every kind.
    Trace {
        #[command(subcommand)]
        sub: TraceSub,
    },
    /// Start or redeem a pairing session for remote QUIC clients (P13.2).
    Pair {
        #[command(subcommand)]
        sub: PairSub,
    },
    /// One-shot prompt: connect to the daemon, send `text`, drain to completion, exit.
    Run {
        /// The user prompt.
        text: String,
        /// Emit JSON-Lines stream of every IPC event.
        #[arg(long)]
        json: bool,
        /// Remote daemon URL (`origin://host:port#fingerprint`).
        #[arg(long)]
        remote: Option<String>,
        /// Optional bearer token for remote auth.
        #[arg(long)]
        bearer: Option<String>,
        /// Model override.
        #[arg(long)]
        model: Option<String>,
    },
    /// Daemon usage snapshot (tokens in/out per provider/model).
    Usage,
    /// Manage persisted sessions.
    Sessions {
        #[command(subcommand)]
        sub: SessionsSub,
    },
    /// Manage stored provider credentials.
    Keyring {
        #[command(subcommand)]
        sub: KeyringSub,
    },
    /// Import a session/skill set from another harness (P14.B.7).
    Import(crate::import::ImportArgs),
    /// List and describe known providers from the builtin catalog.
    Providers {
        #[command(subcommand)]
        sub: ProvidersSub,
    },
    /// Interactive first-time setup. Picks primary / backup / subagent
    /// providers and models, captures credentials, and writes
    /// `~/.origin/config.toml`. Re-running overwrites the existing config.
    Init,
}

#[derive(Subcommand)]
pub enum ProvidersSub {
    /// List every catalog entry (id, display name, wire, auth, capabilities).
    Ls,
    /// Print one provider's full config.
    Describe { id: String },
}

#[derive(Subcommand)]
pub enum TraceSub {
    /// Print spans matching the given filters.
    Query(TraceQuery),
}

#[derive(Subcommand)]
pub enum SessionsSub {
    /// List recent sessions (most-recent first).
    Ls,
    /// Resume a session by id (currently a no-op acknowledgement).
    Resume { session_id: String },
    /// Delete a session and all its messages.
    Rm { session_id: String },
}

#[derive(Subcommand)]
pub enum KeyringSub {
    /// Add or overwrite a provider secret.
    Add {
        provider: String,
        account: String,
        /// The secret value; read from stdin if `-`.
        secret: String,
    },
    /// List accounts for a provider.
    List { provider: String },
    /// Remove a provider account secret.
    Remove { provider: String, account: String },
    /// Launch the OAuth flow for an OAuth provider and persist the tokens.
    Login {
        /// Catalog id of an OAuth-backed provider, e.g. "github-copilot" or
        /// "anthropic-oauth".
        provider: String,
        /// Account name to store the tokens under. Defaults to "default".
        #[arg(default_value = "default")]
        account: String,
    },
}

#[derive(Subcommand)]
pub enum PairSub {
    /// Daemon-side: show a 6-digit pairing code.
    Start {
        #[arg(long, default_value_t = 60)]
        ttl_secs: u32,
    },
    /// Client-side: redeem a code against a remote daemon.
    Redeem {
        /// Remote URL: `origin://host:port#fingerprint`.
        url: String,
        /// The 6-digit code shown on the daemon host.
        code: String,
        /// Stable device identifier (defaults to hostname).
        #[arg(long)]
        device_id: Option<String>,
    },
}

/// Build the top-level clap command for `origin` for introspection.
///
/// Used by `xtask manpages` to render man pages via `clap_mangen` without
/// having to depend on the binary crate.
#[must_use]
pub fn main_cli() -> clap::Command {
    <Cli as clap::CommandFactory>::command()
}
