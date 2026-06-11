// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used)]

use origin_tools::proc_supervisor::{SpawnOpts, Supervisor};
use std::time::Duration;

#[tokio::test]
async fn spawn_and_read_output() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "echo hello";
    #[cfg(windows)]
    let cmd = "Write-Output hello";
    let pid = sup.spawn(cmd, &SpawnOpts::default()).unwrap();
    // Wait for output to land in the buffer (PowerShell startup can be slow on Windows).
    tokio::time::sleep(Duration::from_millis(800)).await;
    let chunk = sup.read_since(pid, 0, 4096).unwrap();
    assert!(chunk.bytes.contains("hello"), "got: {:?}", chunk.bytes);
}

#[tokio::test]
async fn read_since_returns_only_new_bytes() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "printf 'aaa\\nbbb\\nccc\\n'";
    #[cfg(windows)]
    let cmd = "'aaa','bbb','ccc' | ForEach-Object { $_ }";
    let pid = sup.spawn(cmd, &SpawnOpts::default()).unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    let first = sup.read_since(pid, 0, 4).unwrap();
    let next = sup.read_since(pid, first.next_offset, 4096).unwrap();
    assert!(!next.bytes.is_empty());
    assert_ne!(first.bytes, next.bytes);
}

#[tokio::test]
async fn timeout_terminates_long_process() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "sleep 60";
    #[cfg(windows)]
    let cmd = "Start-Sleep -Seconds 60";
    let opts = SpawnOpts {
        timeout: Some(Duration::from_millis(300)),
        ..SpawnOpts::default()
    };
    let pid = sup.spawn(cmd, &opts).unwrap();
    // Condition-based wait: poll until the 300ms timeout fires and the kill +
    // reap flips the status to terminal. A fixed 1.5s sleep flaked on loaded
    // CI runners; the generous ceiling never slows a healthy run because the
    // loop exits as soon as the status turns terminal.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut chunk = sup.read_since(pid, 0, 4096).unwrap();
    while !chunk.status.is_terminal() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        chunk = sup.read_since(pid, 0, 4096).unwrap();
    }
    assert!(chunk.status.is_terminal(), "status was {:?}", chunk.status);
}

#[tokio::test]
async fn parallel_processes_have_isolated_buffers() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let (a, b) = ("echo AAA", "echo BBB");
    #[cfg(windows)]
    let (a, b) = ("Write-Output AAA", "Write-Output BBB");
    let pa = sup.spawn(a, &SpawnOpts::default()).unwrap();
    let pb = sup.spawn(b, &SpawnOpts::default()).unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    let ca = sup.read_since(pa, 0, 4096).unwrap();
    let cb = sup.read_since(pb, 0, 4096).unwrap();
    assert!(ca.bytes.contains("AAA"));
    assert!(cb.bytes.contains("BBB"));
    assert!(!ca.bytes.contains("BBB"));
}
