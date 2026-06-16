// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used)]

use filetime::{set_file_mtime, FileTime};
use origin_tools::builtins::glob_tool::{glob_v2, GlobArgs};
use std::fs;
use tempfile::tempdir;

#[test]
fn returns_matches_sorted_by_mtime_desc() {
    let d = tempdir().unwrap();
    let old = d.path().join("old.rs");
    let new = d.path().join("new.rs");
    fs::write(&old, "").unwrap();
    fs::write(&new, "").unwrap();
    // Stamp explicit, distinct mtimes instead of relying on a wall-clock sleep
    // gap between writes (which raced filesystems with coarse mtime resolution
    // and an unstable tie-break when the two writes landed in the same tick).
    // The glob sorts mtime-DESC, so `new.rs` (newer) must precede `old.rs`.
    set_file_mtime(&old, FileTime::from_unix_time(1_000_000, 0)).unwrap();
    set_file_mtime(&new, FileTime::from_unix_time(2_000_000, 0)).unwrap();

    let out = glob_v2(GlobArgs {
        pattern: "**/*.rs".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        head_limit: None,
    })
    .unwrap();
    let arr = out.as_array().unwrap();
    assert!(arr[0].as_str().unwrap().ends_with("new.rs"));
    assert!(arr[1].as_str().unwrap().ends_with("old.rs"));
}

#[test]
fn respects_gitignore() {
    let d = tempdir().unwrap();
    // Create a .git dir so the ignore crate treats this directory as a git
    // repo root and respects .gitignore entries (standard_filters requires
    // a .git marker to enable gitignore processing).
    fs::create_dir(d.path().join(".git")).unwrap();
    fs::write(d.path().join(".gitignore"), "ignored.rs\n").unwrap();
    fs::write(d.path().join("kept.rs"), "").unwrap();
    fs::write(d.path().join("ignored.rs"), "").unwrap();
    let out = glob_v2(GlobArgs {
        pattern: "*.rs".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        head_limit: None,
    })
    .unwrap();
    let arr = out.as_array().unwrap();
    for v in arr {
        assert!(!v.as_str().unwrap().ends_with("ignored.rs"));
    }
}

#[test]
fn head_limit_caps_output() {
    let d = tempdir().unwrap();
    for i in 0..10 {
        fs::write(d.path().join(format!("f{i}.rs")), "").unwrap();
    }
    let out = glob_v2(GlobArgs {
        pattern: "*.rs".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        head_limit: Some(3),
    })
    .unwrap();
    assert_eq!(out.as_array().unwrap().len(), 3);
}
