// SPDX-License-Identifier: Apache-2.0
//! P13.4.3 — `origin sessions`, `origin usage`, `origin keyring` are
//! reachable from the binary and surface clap-style `--help` output.

use std::process::Command;

#[test]
fn sessions_ls_help() {
    let out = Command::new(env!("CARGO_BIN_EXE_origin"))
        .args(["sessions", "ls", "--help"])
        .output()
        .expect("run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!((stdout.into_owned() + &stderr).contains("Usage"));
}

#[test]
fn usage_help() {
    let out = Command::new(env!("CARGO_BIN_EXE_origin"))
        .args(["usage", "--help"])
        .output()
        .expect("run");
    assert!(out.status.success());
}

#[test]
fn keyring_help() {
    let out = Command::new(env!("CARGO_BIN_EXE_origin"))
        .args(["keyring", "--help"])
        .output()
        .expect("run");
    assert!(out.status.success());
}
