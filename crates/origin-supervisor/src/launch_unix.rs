//! POSIX child-launch.

use std::path::Path;
use std::process::ExitStatus;
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug)]
#[allow(dead_code)] // `status` is consumed via the `Debug` impl in supervisor logs.
pub struct ExitInfo {
    pub status: ExitStatus,
}

/// Spawn the daemon binary and block until it exits.
///
/// # Errors
/// Propagates I/O errors from runtime build, spawn, or wait.
pub fn run_child(daemon_path: &Path, forward: &[String]) -> anyhow::Result<ExitInfo> {
    // We deliberately use a fresh single-threaded runtime per child — the
    // supervisor itself does ~zero async work between restarts.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let mut cmd = Command::new(daemon_path);
        cmd.args(forward)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let status = cmd.spawn()?.wait().await?;
        Ok(ExitInfo { status })
    })
}
