#![cfg(all(target_os = "linux", feature = "linux", not(feature = "no-sandbox")))]

use origin_sandbox::{apply, SandboxProfile};
use std::process::Command;
use tempfile::tempdir;

#[test]
fn read_fs_blocks_write_outside_workspace() {
    let dir = tempdir().expect("tempdir");
    std::env::set_current_dir(&dir).expect("chdir");

    let outside = std::env::temp_dir().join("origin-sb-outside.txt");
    let _ = std::fs::remove_file(&outside);
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(format!("echo blocked > {}", outside.display()));
    apply(SandboxProfile::ReadFs, &mut cmd).expect("apply");

    let status = cmd.status().expect("spawn");
    assert!(!status.success(), "expected sandboxed write to fail");
    assert!(!outside.exists(), "outside file should not have been created");
}

#[test]
fn write_cwd_allows_write_inside_workspace() {
    let dir = tempdir().expect("tempdir");
    std::env::set_current_dir(&dir).expect("chdir");

    let inside = dir.path().join("ok.txt");
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(format!("echo ok > {}", inside.display()));
    apply(SandboxProfile::WriteCwd, &mut cmd).expect("apply");
    let status = cmd.status().expect("spawn");
    assert!(status.success());
    assert!(inside.exists());
}

#[test]
fn shell_profile_blocks_inet_socket() {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg("python3 -c 'import socket; s=socket.socket(); s.connect((\"127.0.0.1\", 1))' 2>&1; echo rc=$?");
    apply(SandboxProfile::Shell, &mut cmd).expect("apply");
    let output = cmd.output().expect("spawn");
    let body = String::from_utf8_lossy(&output.stdout);
    assert!(
        body.contains("PermissionError") || body.contains("rc=159") || body.contains("rc=137"),
        "expected blocked socket call, got: {body}"
    );
}
