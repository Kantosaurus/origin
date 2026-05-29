#![allow(clippy::unwrap_used)]

use origin_tools::builtins::monitor::{monitor, MonitorArgs};
use origin_tools::proc_supervisor::{SpawnOpts, Supervisor};

#[tokio::test]
async fn monitor_returns_bytes_and_next_offset() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "echo hello-world";
    #[cfg(windows)]
    let cmd = "Write-Output hello-world";
    let pid = sup.spawn(cmd, &SpawnOpts::default()).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    let v = monitor(
        MonitorArgs {
            pid,
            since_byte: 0,
            max_bytes: 4096,
            wait: false,
        },
        &sup,
    )
    .await
    .unwrap();
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
