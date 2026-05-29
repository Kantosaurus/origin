// SPDX-License-Identifier: Apache-2.0
//! `origin-supervisor` — restarts `origin-daemon` on crash; coordinates session
//! resume tokens across the restart.

use clap::Parser;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

#[cfg(unix)]
mod launch_unix;
#[cfg(unix)]
use launch_unix as launch;

#[cfg(windows)]
mod launch_windows;
#[cfg(windows)]
use launch_windows as launch;

#[derive(Debug, Parser)]
#[command(name = "origin-supervisor", version)]
struct Args {
    /// Path to the `origin-daemon` binary. Falls back to `which origin-daemon`.
    #[arg(long)]
    daemon_path: Option<PathBuf>,

    /// Maximum restarts within a sliding 60-second window before giving up.
    #[arg(long, default_value = "12")]
    max_restarts_per_min: u32,

    /// Arguments forwarded to the daemon (separate with `--`).
    #[arg(last = true)]
    forward: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let daemon_path = args
        .daemon_path
        .or_else(|| which::which("origin-daemon").ok())
        .ok_or_else(|| anyhow::anyhow!("origin-daemon not found"))?;
    info!(path = %daemon_path.display(), "supervisor: starting");

    let mut restarts: Vec<Instant> = Vec::with_capacity(args.max_restarts_per_min as usize);
    loop {
        let now = Instant::now();
        restarts.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
        let restart_count = u32::try_from(restarts.len()).unwrap_or(u32::MAX);
        if restart_count >= args.max_restarts_per_min {
            error!(
                "supervisor: {} restarts in the last 60s exceeds the cap; bailing",
                restarts.len()
            );
            return Err(anyhow::anyhow!("restart storm"));
        }
        let exit = launch::run_child(&daemon_path, &args.forward)?;
        warn!(?exit, "supervisor: daemon exited; will restart");
        restarts.push(Instant::now());
        std::thread::sleep(Duration::from_millis(250));
    }
}
