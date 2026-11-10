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
    assert_eq!(footer(&out), None, "whole file read: no footer");
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
    assert_eq!(
        footer(&out),
        Some((85, 15)),
        "limit-cut window must name the next offset:\n{out}"
    );
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

/// Parses `[N more lines — call Read with offset=K]` off the end of `out`.
fn footer(out: &str) -> Option<(usize, u32)> {
    let last = out.lines().last()?;
    let rest = last.strip_prefix('[')?.strip_suffix(']')?;
    let (n, k) = rest.split_once(" more lines \u{2014} call Read with offset=")?;
    Some((n.parse().ok()?, k.parse().ok()?))
}

#[test]
fn large_read_is_byte_capped_and_pages_without_loss_or_duplication() {
    // 10,000 lines × 100 bytes = 1 MB.
    let dir = tempdir().unwrap();
    let p = dir.path().join("big.txt");
    let body: String = (1..=10_000).map(|i| format!("{i:<99}\n")).collect();
    assert_eq!(body.len(), 1_000_000);
    fs::write(&p, body).unwrap();

    let mut seen: Vec<usize> = Vec::new();
    let mut offset = 0u32;
    loop {
        let out = read_v2(ReadArgs {
            file_path: p.to_string_lossy().into_owned(),
            offset: Some(offset),
            limit: Some(50_000),
            as_: None,
        })
        .unwrap();
        assert!(
            out.len() <= 100_000,
            "page at offset {offset} is {} bytes",
            out.len()
        );
        let page: Vec<usize> = out
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(n, _)| n.trim().parse().unwrap())
            .collect();
        assert_eq!(
            page.first(),
            Some(&(offset as usize + 1)),
            "page must start at offset+1"
        );
        seen.extend(&page);
        match footer(&out) {
            Some((more, next)) => {
                assert_eq!(
                    next as usize,
                    *page.last().unwrap(),
                    "next offset = first line not returned"
                );
                assert_eq!(more, 10_000 - next as usize);
                offset = next;
            }
            None => break,
        }
    }
    assert_eq!(seen, (1..=10_000).collect::<Vec<_>>());
}

#[test]
fn overlong_line_is_truncated_at_2000_chars() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("min.js");
    fs::write(&p, format!("{}\nnext\n", "x".repeat(10_000))).unwrap();
    let out = read_v2(ReadArgs {
        file_path: p.to_string_lossy().into_owned(),
        offset: None,
        limit: None,
        as_: None,
    })
    .unwrap();
    let first = out.lines().next().unwrap();
    assert!(
        first.contains(&"x".repeat(2_000)) && !first.contains(&"x".repeat(2_001)),
        "{first:.80}"
    );
    assert!(
        first.contains("10000 chars"),
        "truncation must be marked: {}",
        &first[first.len() - 40..]
    );
    assert!(out.contains("     2\tnext"));
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
