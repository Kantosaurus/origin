// SPDX-License-Identifier: Apache-2.0
use std::path::PathBuf;
use std::process::Command;

fn cargo_bin() -> String {
    env!("CARGO").to_string()
}

/// Resolve the workspace root from `CARGO_MANIFEST_DIR` (== `xtask/`).
fn workspace_root() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir)
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

#[test]
fn lint_spawn_passes_on_clean_fixture() {
    let status = Command::new(cargo_bin())
        .current_dir(workspace_root())
        .args([
            "run",
            "--quiet",
            "-p",
            "xtask",
            "--",
            "lint-spawn",
            "--root",
            "xtask/tests/fixtures/clean_spawn",
        ])
        .status()
        .expect("xtask run");
    assert!(status.success(), "clean_spawn fixture should pass lint");
}

#[test]
fn lint_spawn_fails_on_dirty_fixture() {
    let status = Command::new(cargo_bin())
        .current_dir(workspace_root())
        .args([
            "run",
            "--quiet",
            "-p",
            "xtask",
            "--",
            "lint-spawn",
            "--root",
            "xtask/tests/fixtures/dirty_spawn",
        ])
        .status()
        .expect("xtask run");
    assert!(
        !status.success(),
        "dirty_spawn fixture should fail lint with non-zero exit"
    );
}
