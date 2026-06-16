// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used)]

use origin_tools::builtins::bash::{bash_v2, BashArgs};
use origin_tools::proc_supervisor::Supervisor;
use std::time::Duration;

#[tokio::test]
async fn foreground_returns_full_output() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "echo hello";
    #[cfg(windows)]
    let cmd = "Write-Output hello";
    let out = bash_v2(
        BashArgs {
            command: cmd.into(),
            timeout: None,
            cwd: None,
            env: vec![],
            run_in_background: false,
        },
        &sup,
    )
    .await
    .unwrap();
    assert_eq!(out["status"], "exited");
    assert!(
        out["stdout"].as_str().unwrap().contains("hello"),
        "stdout: {:?}",
        out["stdout"]
    );
    assert_eq!(out["exit_code"], 0);
}

#[tokio::test]
async fn background_returns_pid_immediately() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "sleep 1";
    #[cfg(windows)]
    let cmd = "Start-Sleep -Seconds 1";
    let started = std::time::Instant::now();
    let out = bash_v2(
        BashArgs {
            command: cmd.into(),
            timeout: None,
            cwd: None,
            env: vec![],
            run_in_background: true,
        },
        &sup,
    )
    .await
    .unwrap();
    // The command would take ~1s if we waited for it. Returning in well under
    // that proves run_in_background does not block on completion. The bound is
    // loosened from 500ms to 900ms so a slow PowerShell cold-start on a loaded
    // CI runner doesn't flake, while still being strictly below the 1s command.
    assert!(
        started.elapsed() < Duration::from_millis(900),
        "background spawn blocked for {:?}, expected well under the 1s command",
        started.elapsed()
    );
    assert_eq!(out["status"], "started");
    assert!(out["pid"].as_u64().is_some());
}

#[tokio::test]
async fn timeout_returns_timed_out_status() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "sleep 5";
    #[cfg(windows)]
    let cmd = "Start-Sleep -Seconds 5";
    let out = bash_v2(
        BashArgs {
            command: cmd.into(),
            timeout: Some(1),
            cwd: None,
            env: vec![],
            run_in_background: false,
        },
        &sup,
    )
    .await
    .unwrap();
    assert_eq!(out["status"], "timed_out");
}
