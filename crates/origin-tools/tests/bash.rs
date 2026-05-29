// SPDX-License-Identifier: Apache-2.0
use origin_tools::builtins::bash::{bash_v2, BashArgs};
use origin_tools::proc_supervisor::Supervisor;

#[tokio::test]
async fn echoes_and_returns_stdout() {
    // Portable echo: print a fixed string via the platform shell.
    #[cfg(unix)]
    let cmd = "printf 'hello-bash'";
    #[cfg(windows)]
    let cmd = "Write-Output 'hello-bash'";

    let sup = Supervisor::new();
    let out = bash_v2(
        BashArgs {
            command: cmd.into(),
            timeout: None,
            cwd: None,
            env: vec![],
            run_in_background: false,
        },
        &sup,
    )
    .await
    .expect("bash ok");
    assert!(
        out["stdout"].as_str().unwrap_or("").contains("hello-bash"),
        "got: {out:?}"
    );
    assert_eq!(out["exit_code"], 0);
}

#[tokio::test]
async fn non_zero_exit_propagates() {
    #[cfg(unix)]
    let cmd = "exit 7";
    #[cfg(windows)]
    let cmd = "exit 7";

    let sup = Supervisor::new();
    let out = bash_v2(
        BashArgs {
            command: cmd.into(),
            timeout: None,
            cwd: None,
            env: vec![],
            run_in_background: false,
        },
        &sup,
    )
    .await
    .expect("bash ran");
    assert_eq!(out["exit_code"], 7, "expected exit 7, got {out:?}");
}
