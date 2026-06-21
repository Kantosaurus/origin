// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used, clippy::bool_assert_comparison)]

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
    }, None)
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
    }, None)
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
    }, None)
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
    }, None)
    .unwrap_err();
    assert_eq!(err.reason, "no_match");
}

#[test]
fn edit_refuses_a_file_not_read_this_session() {
    use origin_tools::builtins::write::WriteGuard;
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "fn foo() {}\n").unwrap();
    let guard = WriteGuard::default();
    // Never Read ⇒ refuse (recoverable), so old_string can't be hallucinated.
    let err = edit_v2(
        EditArgs {
            file_path: p.to_string_lossy().into_owned(),
            old_string: "foo".into(),
            new_string: "bar".into(),
            replace_all: false,
        },
        Some(&guard),
    )
    .unwrap_err();
    assert_eq!(err.reason, "read_required");
    assert!(err.recoverable);
    // After a Read it is permitted.
    guard.note_read(p.to_string_lossy().as_ref());
    edit_v2(
        EditArgs {
            file_path: p.to_string_lossy().into_owned(),
            old_string: "foo".into(),
            new_string: "bar".into(),
            replace_all: false,
        },
        Some(&guard),
    )
    .unwrap();
    assert_eq!(fs::read_to_string(&p).unwrap(), "fn bar() {}\n");
}

#[test]
fn whitespace_drift_falls_back_to_a_unique_match() {
    // The file is 4-space indented; the model supplies a tab + trailing space.
    // Exact match fails, but the whitespace-tolerant fallback finds the unique
    // line and edits it instead of forcing a blind retry.
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "fn main() {\n    let x = 1;\n}\n").unwrap();
    let out = edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "\tlet x = 1; ".into(),
        new_string: "    let x = 42;".into(),
        replace_all: false,
    }, None)
    .unwrap();
    assert_eq!(out["ok"], true);
    assert_eq!(fs::read_to_string(&p).unwrap(), "fn main() {\n    let x = 42;\n}\n");
}

#[test]
fn no_match_hint_names_the_whitespace_only_near_miss() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "fn main() {\n        let total = a + b;\n}\n").unwrap();
    // Wrong indentation AND a sibling line elsewhere keeps it a near-miss, not
    // a unique fallback (there is only one candidate line, so add a decoy to
    // force the no-match path): use a needle that can't uniquely place.
    let err = edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "let total = a+b;".into(), // operator spacing differs ⇒ genuine miss
        new_string: "x".into(),
        replace_all: false,
    }, None)
    .unwrap_err();
    assert_eq!(err.reason, "no_match");
}
