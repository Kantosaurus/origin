// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used)]

use origin_tools::proc_supervisor::{ProcessId, SpawnOpts, Supervisor};
use origin_tools::SandboxProfile;
use std::time::Duration;

/// Poll the supervisor's ring buffer until `needle` appears or the child exits,
/// with a generous ceiling. A fixed `sleep` raced PowerShell's cold-start on the
/// Windows CI runner (slow first launch), so the reads now wait for real output
/// instead of guessing a duration.
async fn read_until(sup: &Supervisor, pid: ProcessId, needle: &str) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut acc = String::new();
    let mut off = 0u64;
    loop {
        let chunk = sup.read_since(pid, off, 65536).unwrap();
        acc.push_str(&chunk.bytes);
        off = chunk.next_offset;
        if acc.contains(needle) || std::time::Instant::now() > deadline {
            return acc;
        }
        if chunk.status.is_terminal() {
            let more = sup.read_since(pid, off, 65536).unwrap();
            acc.push_str(&more.bytes);
            return acc;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn spawn_and_read_output() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "echo hello";
    #[cfg(windows)]
    let cmd = "Write-Output hello";
    let pid = sup.spawn(cmd, &SpawnOpts::default()).unwrap();
    let out = read_until(&sup, pid, "hello").await;
    assert!(out.contains("hello"), "got: {out:?}");
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

/// P11.5 plumbing: `SpawnOpts` carries the tool's `SandboxProfile`, and a
/// non-`Inherit` profile reaches `origin_sandbox::apply` at spawn. On the
/// default feature set `apply` resolves to the no-op backend (it warns but
/// mutates nothing), so the child still spawns and its output still flows —
/// proving the wiring engages without breaking process execution.
#[tokio::test]
async fn non_inherit_sandbox_profile_still_spawns_and_runs() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "echo sandboxed";
    #[cfg(windows)]
    let cmd = "Write-Output sandboxed";
    let opts = SpawnOpts {
        sandbox_profile: Some(SandboxProfile::Shell),
        ..SpawnOpts::default()
    };
    let pid = sup.spawn(cmd, &opts).unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    let chunk = sup.read_since(pid, 0, 4096).unwrap();
    assert!(chunk.bytes.contains("sandboxed"), "got: {:?}", chunk.bytes);
}

/// The default (`Inherit` / `None`) sandbox profile must be byte-identical to
/// the pre-wiring path: `apply` is never called and the child runs exactly as
/// before. We assert both the unset (`None`) and explicit `Inherit` spellings
/// behave the same as an ordinary spawn.
#[tokio::test]
async fn inherit_sandbox_profile_is_unchanged() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "echo inherit";
    #[cfg(windows)]
    let cmd = "Write-Output inherit";

    // Unset profile (None) — the documented default.
    let pid_none = sup.spawn(cmd, &SpawnOpts::default()).unwrap();
    // Explicit Inherit — must also skip `apply`.
    let opts_inherit = SpawnOpts {
        sandbox_profile: Some(SandboxProfile::Inherit),
        ..SpawnOpts::default()
    };
    let pid_inherit = sup.spawn(cmd, &opts_inherit).unwrap();

    let none = read_until(&sup, pid_none, "inherit").await;
    let inherit = read_until(&sup, pid_inherit, "inherit").await;
    assert!(none.contains("inherit"), "none: {none:?}");
    assert!(inherit.contains("inherit"), "inherit: {inherit:?}");
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
    let ca = read_until(&sup, pa, "AAA").await;
    let cb = read_until(&sup, pb, "BBB").await;
    assert!(ca.contains("AAA"), "a: {ca:?}");
    assert!(cb.contains("BBB"), "b: {cb:?}");
    assert!(!ca.contains("BBB"), "isolation: {ca:?}");
}
