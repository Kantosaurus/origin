//! Canary suite for the CRLF Edit failure class.

use origin_tools::builtins::edit::{edit_v2, EditArgs};
use std::fs;
use tempfile::tempdir;

fn write_with_eol(p: &std::path::Path, body: &[u8]) {
    fs::write(p, body).unwrap();
}

#[test]
fn edit_lf_needle_against_crlf_file_succeeds() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("crlf.rs");
    write_with_eol(&p, b"line1\r\nfoo\r\nline3\r\n");
    let out = edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "foo".into(),
        new_string: "bar".into(),
        replace_all: false,
    })
    .unwrap();
    assert_eq!(out["ok"], true);
    let bytes = fs::read(&p).unwrap();
    assert_eq!(bytes, b"line1\r\nbar\r\nline3\r\n");
}

#[test]
fn edit_lf_needle_against_cr_only_file_succeeds() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("cr.rs");
    write_with_eol(&p, b"line1\rfoo\rline3\r");
    edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "foo".into(),
        new_string: "bar".into(),
        replace_all: false,
    })
    .unwrap();
    assert_eq!(fs::read(&p).unwrap(), b"line1\rbar\rline3\r");
}

#[test]
fn edit_preserves_mixed_eol_byte_for_byte() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("mixed.rs");
    write_with_eol(&p, b"a\r\nb\nfoo\r\nc\r");
    edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "foo".into(),
        new_string: "bar".into(),
        replace_all: false,
    })
    .unwrap();
    assert_eq!(fs::read(&p).unwrap(), b"a\r\nb\nbar\r\nc\r");
}

#[test]
fn write_preserves_eol_when_appending_via_edit() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("file.rs");
    write_with_eol(&p, b"a\r\nb\r\n");
    edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "b".into(),
        new_string: "b\nINSERTED".into(),
        replace_all: false,
    })
    .unwrap();
    // Inserted line inherits CRLF from preceding line.
    assert_eq!(fs::read(&p).unwrap(), b"a\r\nb\r\nINSERTED\r\n");
}

#[test]
fn write_preserves_existing_file_crlf() {
    use origin_tools::builtins::write::{write_v2, WriteArgs, WriteGuard};
    let dir = tempdir().unwrap();
    let p = dir.path().join("crlf.txt");
    fs::write(&p, b"a\r\nb\r\n").unwrap();
    let guard = WriteGuard::default();
    guard.note_read(p.to_string_lossy().as_ref());
    write_v2(WriteArgs {
        file_path: p.to_string_lossy().into_owned(),
        content: "x\ny\nz\n".into(),
        force: false,
    }, &guard).unwrap();
    assert_eq!(fs::read(&p).unwrap(), b"x\r\ny\r\nz\r\n");
}
