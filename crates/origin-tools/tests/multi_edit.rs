#![allow(clippy::unwrap_used)]

use origin_tools::builtins::multi_edit::{multi_edit, EditOp, MultiEditArgs};
use std::fs;
use tempfile::tempdir;

#[test]
fn applies_edits_in_order_atomically() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "alpha\nbeta\ngamma\n").unwrap();
    let out = multi_edit(&MultiEditArgs {
        file_path: p.to_string_lossy().into_owned(),
        edits: vec![
            EditOp {
                old: "alpha".into(),
                new: "A".into(),
                replace_all: false,
            },
            EditOp {
                old: "beta".into(),
                new: "B".into(),
                replace_all: false,
            },
        ],
    })
    .unwrap();
    assert_eq!(out["ok"], true);
    assert_eq!(out["applied"], 2);
    assert_eq!(fs::read_to_string(&p).unwrap(), "A\nB\ngamma\n");
}

#[test]
fn failure_mid_sequence_does_not_partially_write() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "x\ny\nz\n").unwrap();
    let err = multi_edit(&MultiEditArgs {
        file_path: p.to_string_lossy().into_owned(),
        edits: vec![
            EditOp {
                old: "x".into(),
                new: "X".into(),
                replace_all: false,
            },
            EditOp {
                old: "MISSING".into(),
                new: "?".into(),
                replace_all: false,
            },
        ],
    })
    .unwrap_err();
    assert_eq!(err.reason, "no_match");
    assert_eq!(fs::read_to_string(&p).unwrap(), "x\ny\nz\n");
}

#[test]
fn crlf_preserved_across_multiple_edits() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("crlf.rs");
    fs::write(&p, b"a\r\nb\r\nc\r\n").unwrap();
    multi_edit(&MultiEditArgs {
        file_path: p.to_string_lossy().into_owned(),
        edits: vec![
            EditOp {
                old: "a".into(),
                new: "A".into(),
                replace_all: false,
            },
            EditOp {
                old: "b".into(),
                new: "B".into(),
                replace_all: false,
            },
        ],
    })
    .unwrap();
    assert_eq!(fs::read(&p).unwrap(), b"A\r\nB\r\nc\r\n");
}
