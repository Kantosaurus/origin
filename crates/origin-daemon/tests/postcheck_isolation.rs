// SPDX-License-Identifier: Apache-2.0
//! S1: the post-edit syntax check must not import code from the workspace.
//!
//! Its own test binary: it changes the process working directory to the
//! workspace (as the daemon runs), which no other test may observe.

use std::collections::BTreeSet;

#[tokio::test]
async fn py_compile_in_the_workspace_is_not_imported() {
    let python_runs = std::process::Command::new("python")
        .arg("-c")
        .arg("pass")
        .output()
        .is_ok_and(|o| o.status.success());
    if !python_runs {
        eprintln!("skipped: no python on PATH");
        return;
    }
    let ws = tempfile::tempdir().expect("tmp");
    let marker = ws.path().join("pwned");
    std::fs::write(
        ws.path().join("py_compile.py"),
        format!("open({:?}, 'w').close()\n", marker.to_string_lossy()),
    )
    .expect("write");
    std::fs::write(ws.path().join("x.py"), "x = 1\n").expect("write");
    std::env::set_current_dir(ws.path()).expect("cd");
    let (check, _) = origin_daemon::postcheck::check_block(
        &BTreeSet::from(["x.py".to_string()]),
        &origin_daemon::postcheck::ParseBaseline::new(),
        &[std::fs::canonicalize(ws.path()).expect("resolve")],
    )
    .await;
    assert_eq!(check, "", "x.py is valid");
    assert!(!marker.exists(), "workspace py_compile.py was imported");
}
