// SPDX-License-Identifier: Apache-2.0
//! Windows child-launch — child wrapped in a Job Object so the supervisor's
//! own exit terminates the daemon (full Job-object association deferred to a
//! follow-up; P12 ships the restart-on-exit loop).

use std::path::Path;
use std::process::ExitStatus;
use std::process::Stdio;
use tokio::process::Command;

/// `CREATE_NO_WINDOW`: run the console-subsystem daemon child without giving it
/// a console window of its own. The supervisor is launched windowless by the
/// CLI (`CREATE_NO_WINDOW`), so a console-subsystem child spawned with no flags
/// would otherwise allocate a *new* console window — the stray terminal. This
/// flag suppresses it while keeping stdout/stderr inherited into the
/// supervisor's (redirected) handles.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        // Job-object association is left to a follow-up: P12 ships the
        // restart-on-exit loop; full kill-on-supervisor-exit semantics are
        // not the gate.
        let mut cmd = Command::new(daemon_path);
        cmd.args(forward)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .creation_flags(CREATE_NO_WINDOW);
        let status = cmd.spawn()?.wait().await?;
        Ok(ExitInfo { status })
    })
}
