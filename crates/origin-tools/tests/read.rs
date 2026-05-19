use origin_tools::builtins::read::read_tool;
use std::io::Write;

#[test]
fn reads_an_existing_file() {
    let mut f = tempfile::NamedTempFile::new().expect("create tempfile");
    f.write_all(b"hello world").expect("write tempfile");
    let path = f.path().to_str().expect("path utf8");
    let out = read_tool(path).expect("read should succeed");
    assert_eq!(out, "hello world");
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
