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

/// Hard-kill on drop: when the foreground `bash_v2` future is dropped mid-flight
/// (the daemon's interrupt path drops the whole turn future via `select!`), the
/// still-running child must be `SIGKILLed` — not left to run to completion in the
/// detached supervisor task. We race a long-running (no-timeout) command against
/// a short delay, drop the future, then assert the supervisor reports the child
/// terminated promptly. Before the kill-on-drop guard the child kept running for
/// the full 60s and the slot stayed `Running`.
#[tokio::test]
async fn foreground_drop_kills_running_child() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "sleep 60";
    #[cfg(windows)]
    let cmd = "Start-Sleep -Seconds 60";

    // We need the pid the foreground call spawns so we can probe the supervisor
    // after dropping the future. The supervisor hands out pids sequentially from
    // 1, and this is the first (and only) spawn on a fresh supervisor, so pid 1.
    let fut = bash_v2(
        BashArgs {
            command: cmd.into(),
            timeout: None,
            cwd: None,
            env: vec![],
            run_in_background: false,
        },
        &sup,
    );
    // Drive the future just long enough to spawn the child, then drop it. We
    // race it against a short timer; the `select!` dropping the loser drops the
    // bash future at its poll-loop sleep await.
    tokio::select! {
        _ = fut => panic!("60s command should not have completed in 300ms"),
        () = tokio::time::sleep(Duration::from_millis(300)) => {}
    };
    // `fut` is now dropped. The kill-on-drop guard must have terminated pid 1.
    let pid = 1u32;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut chunk = sup.read_since(pid, 0, 4096).unwrap();
    while !chunk.status.is_terminal() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
        chunk = sup.read_since(pid, 0, 4096).unwrap();
    }
    assert!(
        chunk.status.is_terminal(),
        "dropping the foreground bash future must hard-kill the child; status was {:?}",
        chunk.status
    );
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
