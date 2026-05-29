// SPDX-License-Identifier: Apache-2.0
//! Regression guard for the Windows main-thread stack overflow.
//!
//! The async entrypoint's top-level future is a single large state machine.
//! `block_on` materializes that whole future on the stack *before* polling
//! it, and in a debug build it exceeds Windows' default 1 MiB main-thread
//! stack — so the process aborts with `STATUS_STACK_OVERFLOW` (0xC000_00FD)
//! before doing any work, even for `--version`. Linux's 8 MiB default main
//! stack hides it. Driving the runtime on a dedicated large-stack thread
//! fixes it; this test execs the built binary and asserts it exits cleanly.
use std::process::Command;

#[test]
fn version_does_not_overflow_stack() {
    let out = Command::new(env!("CARGO_BIN_EXE_origin"))
        .arg("--version")
        // Keep the probe hermetic: no network update check, no onboarding.
        .env("ORIGIN_NO_UPDATE", "1")
        .env("ORIGIN_SKIP_INIT", "1")
        .output()
        .expect("spawn origin --version");

    // 0xC000_00FD == STATUS_STACK_OVERFLOW. On Windows a stack overflow exits
    // the process with this code; assert we never see it (clearer than the
    // generic success check below when this specific regression returns).
    #[cfg(windows)]
    assert_ne!(
        out.status.code(),
        Some(0xC000_00FD_u32 as i32),
        "origin --version aborted with STATUS_STACK_OVERFLOW"
    );

    assert!(
        out.status.success(),
        "origin --version exited with {:?}; stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("origin"),
        "expected a version banner containing \"origin\", got: {stdout}"
    );
}
