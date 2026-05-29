// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used, clippy::format_collect, clippy::panic)]

use origin_tools::builtins::read::{read_v2, ReadArgs};
use std::fs;
use tempfile::tempdir;

#[test]
fn returns_line_numbered_chunks() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "first\nsecond\nthird\n").unwrap();
    let out = read_v2(ReadArgs {
        file_path: p.to_string_lossy().into_owned(),
        offset: None,
        limit: None,
        as_: None,
    })
    .unwrap();
    assert!(out.contains("     1\tfirst"));
    assert!(out.contains("     2\tsecond"));
    assert!(out.contains("     3\tthird"));
}

#[test]
fn respects_offset_and_limit() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    let body: String = (1..=100).map(|i| format!("line {i}\n")).collect();
    fs::write(&p, body).unwrap();
    let out = read_v2(ReadArgs {
        file_path: p.to_string_lossy().into_owned(),
        offset: Some(10),
        limit: Some(5),
        as_: None,
    })
    .unwrap();
    assert!(out.contains("    11\tline 11"));
    assert!(out.contains("    15\tline 15"));
    assert!(!out.contains("line 16"));
}

#[test]
fn default_limit_is_1000() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    let body: String = (1..=1500).map(|i| format!("L{i}\n")).collect();
    fs::write(&p, body).unwrap();
    let out = read_v2(ReadArgs {
        file_path: p.to_string_lossy().into_owned(),
        offset: None,
        limit: None,
        as_: None,
    })
    .unwrap();
    assert!(out.contains("\tL1000"));
    assert!(!out.contains("\tL1001"));
}

#[test]
fn errors_on_missing_file() {
    let err = read_v2(ReadArgs {
        file_path: "/no/such/file".into(),
        offset: None,
        limit: None,
        as_: None,
    })
    .unwrap_err();
    assert_eq!(err.class, origin_tools::ErrClass::Io);
}

#[test]
fn refuses_to_follow_symlink() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target.txt");
    fs::write(&target, "secret").unwrap();
    let link = dir.path().join("link.txt");

    #[cfg(windows)]
    let link_result = std::os::windows::fs::symlink_file(&target, &link);
    #[cfg(unix)]
    let link_result = std::os::unix::fs::symlink(&target, &link);

    if let Err(e) = &link_result {
        let raw = e.raw_os_error().unwrap_or(0);
        if e.kind() == std::io::ErrorKind::PermissionDenied || raw == 1314 {
            eprintln!("SKIP refuses_to_follow_symlink: insufficient privileges to create symlink (needs admin or Developer Mode on Windows): {e}");
            return;
        }
        panic!("unexpected symlink creation error: {e}");
    }

    let err = read_v2(ReadArgs {
        file_path: link.to_string_lossy().into_owned(),
        offset: None,
        limit: None,
        as_: None,
    })
    .expect_err("must refuse to read through symlink");
    assert_eq!(err.class, origin_tools::ErrClass::Validation);
    assert!(err.reason.contains("symlink"), "reason was {:?}", err.reason);
}
