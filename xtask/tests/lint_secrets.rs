// SPDX-License-Identifier: Apache-2.0
use std::path::PathBuf;
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn xtask_lint(path: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "xtask", "--", "lint-secrets", "--path"])
        .arg(path)
        .output()
        .expect("spawn xtask")
}

#[test]
fn clean_fixture_passes() {
    let out = xtask_lint(&fixture("clean.rs"));
    assert!(
        out.status.success(),
        "clean fixture should pass; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn dirty_fixture_fails() {
    let out = xtask_lint(&fixture("dirty.rs"));
    assert!(
        !out.status.success(),
        "dirty fixture should fail; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("api_key") || stderr.contains("ApiKey"),
        "expected api_key violation in stderr: {stderr}"
    );
}
