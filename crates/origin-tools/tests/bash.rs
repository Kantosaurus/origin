use origin_tools::builtins::bash::bash_tool;

#[tokio::test]
async fn echoes_and_returns_stdout() {
    // Portable echo: print a fixed string via the platform shell.
    #[cfg(unix)]
    let cmd = "printf 'hello-bash'";
    #[cfg(windows)]
    let cmd = "Write-Host -NoNewline 'hello-bash'";

    let out = bash_tool(cmd).await.expect("bash ok");
    assert!(out.stdout.contains("hello-bash"), "got: {out:?}");
    assert_eq!(out.exit_code, 0);
}

#[tokio::test]
async fn non_zero_exit_propagates() {
    #[cfg(unix)]
    let cmd = "exit 7";
    #[cfg(windows)]
    let cmd = "exit 7";

    let out = bash_tool(cmd).await.expect("bash ran");
    assert_eq!(out.exit_code, 7, "expected exit 7, got {out:?}");
}
