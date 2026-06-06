// SPDX-License-Identifier: Apache-2.0
//! `origin lsp …` subcommand handlers.
//!
//! Surfaces the builtin [`origin_lspfleet`] server registry (opencode-style
//! "auto-installing LSP fleet"). `origin lsp ls` lists the 40+ servers; `origin
//! lsp ensure <ext>` reports install/launch status and, when `ORIGIN_LSP_AUTO`
//! is set and the binary is missing, **runs the registry install command** (an
//! explicit opt-in side effect). Default (flag unset) ⇒ a pure, read-only
//! report. Autonomous server *spawn* is daemon-owned and stays deferred.
#![allow(clippy::missing_errors_doc, clippy::unnecessary_wraps)]

use anyhow::Result;

/// Dispatch an `origin lsp …` subcommand.
///
/// # Errors
/// Never errors today (the only subcommand is a read-only listing), but returns
/// `Result` so future fallible subcommands fit without a signature change.
pub fn run(sub: &crate::cli_def::LspSub) -> Result<()> {
    match sub {
        crate::cli_def::LspSub::Ls => {
            ls();
            Ok(())
        }
        crate::cli_def::LspSub::Ensure { ext } => {
            ensure(ext);
            Ok(())
        }
    }
}

/// Resolve the server for `ext` and report its install/launch status.
///
/// This is intentionally non-spawning: it prints whether the launch binary is
/// already on `PATH`, the would-launch command, and the install hint. When the
/// `ORIGIN_LSP_AUTO` env flag is set it additionally prints the launch the
/// daemon would perform — the autonomous auto-install/spawn itself lives behind
/// the same flag in the daemon (deferred from this CLI surface).
fn ensure(ext: &str) {
    let normalized = ext.trim_start_matches('.');
    let Some(server) = origin_lspfleet::server_for_extension(normalized) else {
        println!("no builtin LSP server handles `.{normalized}` files");
        return;
    };
    // `launch` is a single shell command string; the first whitespace-delimited
    // token is the program we probe on PATH.
    let program = server.launch.split_whitespace().next().unwrap_or("");
    let on_path = which::which(program).is_ok();

    println!("language : {}", server.language);
    println!("server   : {}", server.server_id);
    println!("launch   : {}", server.launch);
    println!(
        "status   : {}",
        if on_path {
            "binary found on PATH"
        } else {
            "binary NOT found on PATH"
        }
    );
    if !on_path {
        println!("install  : {}", server.install);
    }

    // Auto-install (opencode-style): gated behind `ORIGIN_LSP_AUTO`. With the
    // flag set and the binary missing, actually run the registry's install
    // command (an explicit opt-in side effect). Spawning/launching the server is
    // daemon-owned and stays deferred. Default (flag unset) ⇒ pure report.
    if std::env::var_os("ORIGIN_LSP_AUTO").is_some() {
        if on_path {
            println!("auto     : ORIGIN_LSP_AUTO set; `{program}` already on PATH (no install needed)");
        } else {
            println!("auto     : ORIGIN_LSP_AUTO set; running install: {}", server.install);
            match run_install(server.install) {
                Ok(status) if status.success() => {
                    println!("auto     : install completed successfully");
                }
                Ok(status) => println!("auto     : install exited unsuccessfully ({status})"),
                Err(e) => println!("auto     : install failed to start: {e}"),
            }
        }
    }
}

/// Run an LSP server's install command through the platform shell. Used only on
/// the explicit `ORIGIN_LSP_AUTO` opt-in path; the command string comes from the
/// builtin [`origin_lspfleet`] registry, not from user input.
fn run_install(install_cmd: &str) -> std::io::Result<std::process::ExitStatus> {
    #[cfg(windows)]
    {
        std::process::Command::new("cmd").arg("/C").arg(install_cmd).status()
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("sh").arg("-c").arg(install_cmd).status()
    }
}

/// Print every server in the builtin registry as a fixed-width table.
fn ls() {
    let servers = origin_lspfleet::registry();
    println!(
        "{:<14} {:<28} {:<22} EXTENSIONS",
        "LANGUAGE", "SERVER", "LAUNCH"
    );
    for s in servers {
        let exts = s.extensions.join(",");
        println!(
            "{:<14} {:<28} {:<22} {exts}",
            s.language, s.server_id, s.launch
        );
    }
    println!("\n{} servers in the builtin registry.", servers.len());
    println!("(install a server with its `install` command; origin never auto-installs without consent.)");
}

#[cfg(test)]
mod tests {
    /// A known extension resolves to a server (with or without a leading dot);
    /// an unknown one does not. Pins the `ensure` resolution behaviour without
    /// shelling out to `which`.
    #[test]
    fn ensure_resolves_known_and_unknown_extensions() {
        assert!(origin_lspfleet::server_for_extension("rs").is_some());
        assert!(origin_lspfleet::server_for_extension(".rs".trim_start_matches('.')).is_some());
        assert!(origin_lspfleet::server_for_extension("no-such-ext").is_none());
    }
}
