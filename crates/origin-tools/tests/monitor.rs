// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used)]

use origin_tools::builtins::monitor::{monitor, MonitorArgs};
use origin_tools::proc_supervisor::{SpawnOpts, Supervisor};
use std::time::Duration;

/// Poll `monitor` (wait:false) from offset 0 until the cumulative output
/// contains `needle` or the process is terminal, with a generous ceiling.
/// A single fixed `sleep` raced PowerShell's cold-start on the Windows CI
/// runner (slow first launch), so the read now waits for real output instead
/// of guessing a duration. Mirrors `proc_supervisor::read_until`.
async fn monitor_until(sup: &Supervisor, pid: u32, needle: &str) -> serde_json::Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let v = monitor(
            MonitorArgs {
                pid,
                since_byte: 0,
                max_bytes: 4096,
                wait: false,
            },
            sup,
        )
        .await
        .unwrap();
        let has_needle = v["bytes"].as_str().is_some_and(|s| s.contains(needle));
        let terminal = v["status"].as_str() != Some("running");
        if has_needle || terminal || std::time::Instant::now() > deadline {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn monitor_returns_bytes_and_next_offset() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "echo hello-world";
    #[cfg(windows)]
    let cmd = "Write-Output hello-world";
    let pid = sup.spawn(cmd, &SpawnOpts::default()).unwrap();
    let v = monitor_until(&sup, pid, "hello-world").await;
    assert!(
        v["bytes"].as_str().unwrap().contains("hello-world"),
        "bytes: {:?}",
        v["bytes"]
    );
    assert!(v["next_offset"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn monitor_unknown_pid_errors() {
    let sup = Supervisor::new();
    let err = monitor(
        MonitorArgs {
            pid: 999_999,
            since_byte: 0,
            max_bytes: 1,
            wait: false,
        },
        &sup,
    )
    .await
    .unwrap_err();
    assert_eq!(err.reason, "unknown_pid");
}
