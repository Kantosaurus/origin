#![cfg(all(target_os = "macos", feature = "macos", not(feature = "no-sandbox")))]

use origin_sandbox::{apply, SandboxProfile};
use std::process::Command;
use tempfile::tempdir;

#[test]
fn read_fs_blocks_write_outside_workspace() {
    let dir = tempdir().expect("tempdir");
    std::env::set_current_dir(&dir).expect("chdir");

    let outside = std::env::temp_dir().join("origin-sb-mac-outside.txt");
    let _ = std::fs::remove_file(&outside);
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(format!("echo blocked > {}", outside.display()));
    apply(SandboxProfile::ReadFs, &mut cmd).expect("apply");
    let status = cmd.status().expect("spawn");
    assert!(!status.success(), "sandbox-exec should block write outside cwd");
    assert!(!outside.exists());
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
