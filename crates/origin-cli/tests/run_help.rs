// SPDX-License-Identifier: Apache-2.0
use std::process::Command;

#[test]
fn run_help_lists_json_flag() {
    let out = Command::new(env!("CARGO_BIN_EXE_origin"))
        .args(["run", "--help"])
        .output()
        .expect("run cli");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("--json"),
        "expected --json flag in help: {combined}"
    );
}
