//! P11.4 — Windows backend (AppContainer + restricted Job Object).
//!
//! On Windows the CPU/RAM cap layer must run *after* `CreateProcess`. The
//! `apply()` entry point sets `CREATE_SUSPENDED` on the command; the caller
//! is then expected to invoke `attach_job_object_if_needed` on the spawned
//! child before resuming the main thread.

#![cfg(all(target_os = "windows", feature = "windows"))]

use origin_sandbox::{apply, backend_windows::attach_job_object_if_needed, SandboxProfile};
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn inherit_profile_is_noop() {
    let mut cmd = Command::new("cmd.exe");
    cmd.args(["/C", "echo ok"]);
    apply(SandboxProfile::Inherit, &mut cmd).expect("apply Inherit");
    let status = cmd.status().expect("spawn");
    assert!(status.success(), "Inherit must not affect a normal exit");
}

#[test]
fn shell_profile_sets_create_suspended_then_job_object_terminates_runaway() {
    // Spin loop that never naturally completes within a sensible test window.
    // The Job Object's PER_PROCESS_USER_TIME_LIMIT (60s) must kill it well
    // before the 90s upper bound below.
    let mut cmd = Command::new("cmd.exe");
    cmd.args(["/C", "FOR /L %i IN (1,1,2000000000) DO @rem"]);
    apply(SandboxProfile::Shell, &mut cmd).expect("apply Shell");

    let mut child = cmd.spawn().expect("spawn suspended");
    attach_job_object_if_needed(&mut child).expect("attach job");
    // The child was started CREATE_SUSPENDED; the helper resumes it.
    let start = Instant::now();
    let _ = child.wait();
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(90),
        "JobObject CPU cap should fire before 90s, lived {elapsed:?}"
    );
}
