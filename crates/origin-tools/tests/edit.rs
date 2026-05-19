use origin_tools::builtins::edit::edit_tool;
use std::fs;

#[test]
fn replaces_unique_string() {
    let f = tempfile::NamedTempFile::new().expect("tempfile");
    fs::write(f.path(), "alpha beta gamma").expect("write");
    edit_tool(f.path().to_str().expect("utf8"), "beta", "BETA").expect("edit ok");
    let after = fs::read_to_string(f.path()).expect("read");
    assert_eq!(after, "alpha BETA gamma");
}

#[test]
fn errors_when_old_string_not_found() {
    let f = tempfile::NamedTempFile::new().expect("tempfile");
    fs::write(f.path(), "alpha beta").expect("write");
    let err =
        edit_tool(f.path().to_str().expect("utf8"), "gamma", "GAMMA").expect_err("should error on missing");
    assert!(err.contains("not found"), "got: {err}");
}

#[test]
fn errors_when_old_string_is_ambiguous() {
    let f = tempfile::NamedTempFile::new().expect("tempfile");
    fs::write(f.path(), "abc abc abc").expect("write");
    let err =
        edit_tool(f.path().to_str().expect("utf8"), "abc", "XYZ").expect_err("should error on ambiguous");
    assert!(
        err.contains("ambig") || err.contains("multiple") || err.contains("not unique"),
        "got: {err}"
    );
}
