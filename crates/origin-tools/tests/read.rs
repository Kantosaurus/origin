use origin_tools::builtins::read::read_tool;
use std::io::Write;
#[cfg(windows)]
use std::os::windows::fs::symlink_file;

#[test]
fn reads_an_existing_file() {
    let mut f = tempfile::NamedTempFile::new().expect("create tempfile");
    f.write_all(b"hello world").expect("write tempfile");
    let path = f.path().to_str().expect("path utf8");
    let out = read_tool(path).expect("read should succeed");
    assert_eq!(out, "hello world");
}

#[test]
fn refuses_to_follow_symlink() {
    // Defense-in-depth: a symlink planted in the project tree must not let the
    // Read tool exfiltrate sensitive files outside the intended scope.
    let dir = tempfile::tempdir().expect("create tempdir");
    let target = dir.path().join("target.txt");
    std::fs::write(&target, b"secret payload").expect("write target");
    let link = dir.path().join("link.txt");

    #[cfg(windows)]
    let link_result = symlink_file(&target, &link);
    #[cfg(unix)]
    let link_result = std::os::unix::fs::symlink(&target, &link);

    if let Err(e) = link_result {
        // On Windows, lack of SeCreateSymbolicLinkPrivilege surfaces as raw OS
        // error 1314 ("a required privilege is not held by the client") rather
        // than PermissionDenied. Treat either as a clean SKIP.
        let raw = e.raw_os_error();
        if e.kind() == std::io::ErrorKind::PermissionDenied || raw == Some(1314) {
            eprintln!(
                "SKIP refuses_to_follow_symlink: insufficient privileges to create symlink \
                 (needs admin or Developer Mode on Windows): {e}"
            );
            return;
        }
        panic!("unexpected symlink creation error: {e}");
    }

    let link_path = link.to_str().expect("link path utf8");
    let err = read_tool(link_path).expect_err("symlink read should be rejected");
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::PermissionDenied,
        "expected PermissionDenied, got {err:?}"
    );

    // Reading the real file directly still works.
    let target_path = target.to_str().expect("target path utf8");
    let out = read_tool(target_path).expect("direct read should succeed");
    assert_eq!(out, "secret payload");
}

#[test]
fn missing_file_returns_error() {
    let err =
        read_tool("/this/path/definitely/does/not/exist/origin-test").expect_err("missing file should error");
    let msg = format!("{err}");
    assert!(
        msg.to_ascii_lowercase().contains("not"),
        "error should mention not-found, got: {msg}"
    );
}
