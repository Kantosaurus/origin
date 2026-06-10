// SPDX-License-Identifier: Apache-2.0
use origin_hooks::{ShellPool, ShellSpec};

fn default_spec() -> ShellSpec {
    if cfg!(windows) {
        ShellSpec {
            program: "cmd.exe".into(),
            args: vec!["/Q".into(), "/K".into(), "@echo off".into()],
            read_terminator: 0u8,
        }
    } else {
        ShellSpec {
            program: "/bin/sh".into(),
            args: vec!["-s".into()],
            read_terminator: 0u8,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(windows, ignore = "cmd.exe stdin buffering; covered by integration in P10.6")]
async fn pool_reuses_one_child_across_dispatches() {
    let pool = ShellPool::new(default_spec(), 1).await.expect("pool");
    for i in 0..100usize {
        // The script: echo "hi-{i}" followed by NUL terminator so the pool can frame.
        let script = if cfg!(windows) {
            format!("echo hi-{i}&<NUL set /p=\"\x00\"\r\n")
        } else {
            format!("printf 'hi-{i}\\0'\n")
        };
        let resp = pool.dispatch(&script).await.expect("dispatch");
        assert!(resp.starts_with(&format!("hi-{i}").into_bytes()));
    }
    // Pool size 1 + 100 dispatches → exactly one underlying child must have been spawned.
    assert_eq!(pool.spawn_count(), 1, "no per-event spawns");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(windows, ignore = "cmd.exe stdin buffering; covered by integration in P10.6")]
async fn pool_recreates_dead_child() {
    let pool = ShellPool::new(default_spec(), 1).await.expect("pool");
    let exit_script = if cfg!(windows) {
        "exit\r\n".to_string()
    } else {
        "exit 0\n".to_string()
    };
    // Best-effort: send `exit` and ignore the response (the child closes stdout).
    let _ = pool.dispatch(&exit_script).await;

    // Next dispatch should spawn a fresh child.
    let script = if cfg!(windows) {
        "echo alive&<NUL set /p=\"\x00\"\r\n".to_string()
    } else {
        "printf 'alive\\0'\n".to_string()
    };
    let resp = pool.dispatch(&script).await.expect("dispatch after death");
    assert!(resp.starts_with(b"alive"));
    // At least one respawn must have occurred (the dead child was recreated).
    // The exact count can exceed 2: the `exit` dispatch itself hits StdoutClosed
    // mid-call and triggers a respawn-and-retry (the retry re-runs `exit`, killing
    // that worker too), so a clean run lands at 3 spawns total. Assert the
    // invariant (recreation happened) rather than a brittle exact count.
    assert!(
        pool.spawn_count() >= 2,
        "dead child should have been recreated (>=1 respawn); got {}",
        pool.spawn_count()
    );
}
