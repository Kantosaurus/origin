use origin_tools::builtins::edit::{edit_v2, EditArgs};
use std::fs;
use tempfile::tempdir;

#[test]
fn single_replacement_returns_hunk() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "fn foo() {}\n").unwrap();
    let out = edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "foo".into(),
        new_string: "bar".into(),
        replace_all: false,
    })
    .unwrap();
    assert_eq!(out["ok"], true);
    assert_eq!(out["hunks"][0]["before"].as_str().unwrap().contains("foo"), true);
    assert_eq!(out["hunks"][0]["after"].as_str().unwrap().contains("bar"), true);
    let actual = fs::read_to_string(&p).unwrap();
    assert_eq!(actual, "fn bar() {}\n");
}

#[test]
fn ambiguous_match_without_replace_all_errors() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "foo foo foo\n").unwrap();
    let err = edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "foo".into(),
        new_string: "bar".into(),
        replace_all: false,
    })
    .unwrap_err();
    assert_eq!(err.class, origin_tools::ErrClass::Edit);
    assert_eq!(err.reason, "ambiguous");
}

#[test]
fn replace_all_replaces_every_occurrence() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "foo foo foo\n").unwrap();
    edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "foo".into(),
        new_string: "bar".into(),
        replace_all: true,
    })
    .unwrap();
    assert_eq!(fs::read_to_string(&p).unwrap(), "bar bar bar\n");
}

#[test]
fn no_match_errors_with_edit_no_match() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "hello\n").unwrap();
    let err = edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "missing".into(),
        new_string: "x".into(),
        replace_all: false,
    })
    .unwrap_err();
    assert_eq!(err.reason, "no_match");
}
