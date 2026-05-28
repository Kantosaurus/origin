use origin_tools::builtins::write::{write_v2, WriteArgs, WriteGuard};
use std::fs;
use tempfile::tempdir;

#[test]
fn creates_new_file_without_guard_issue() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("new.txt");
    let guard = WriteGuard::default();
    write_v2(WriteArgs {
        file_path: p.to_string_lossy().into_owned(),
        content: "hello".into(),
        force: false,
    }, &guard)
    .unwrap();
    assert_eq!(fs::read_to_string(&p).unwrap(), "hello");
}

#[test]
fn rejects_overwrite_of_unread_existing_file() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("existing.txt");
    fs::write(&p, "old").unwrap();
    let guard = WriteGuard::default();
    let err = write_v2(WriteArgs {
        file_path: p.to_string_lossy().into_owned(),
        content: "new".into(),
        force: false,
    }, &guard)
    .unwrap_err();
    assert_eq!(err.reason, "read_required");
}

#[test]
fn allows_overwrite_after_marking_read() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("existing.txt");
    fs::write(&p, "old").unwrap();
    let guard = WriteGuard::default();
    guard.note_read(p.to_string_lossy().as_ref());
    write_v2(WriteArgs {
        file_path: p.to_string_lossy().into_owned(),
        content: "new".into(),
        force: false,
    }, &guard).unwrap();
    assert_eq!(fs::read_to_string(&p).unwrap(), "new");
}

#[test]
fn force_true_bypasses_guard() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("existing.txt");
    fs::write(&p, "old").unwrap();
    let guard = WriteGuard::default();
    write_v2(WriteArgs {
        file_path: p.to_string_lossy().into_owned(),
        content: "new".into(),
        force: true,
    }, &guard).unwrap();
}

#[test]
fn preserves_prior_crlf_when_overwriting() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("crlf.txt");
    fs::write(&p, b"a\r\nb\r\n").unwrap();
    let guard = WriteGuard::default();
    guard.note_read(p.to_string_lossy().as_ref());
    write_v2(WriteArgs {
        file_path: p.to_string_lossy().into_owned(),
        content: "x\ny\n".into(),
        force: false,
    }, &guard).unwrap();
    assert_eq!(fs::read(&p).unwrap(), b"x\r\ny\r\n");
}
