// SPDX-License-Identifier: Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::case_sensitive_file_extension_comparisons
)]

use origin_tools::builtins::grep_tool::{grep_v2, GrepArgs, OutputMode};
use std::fs;
use tempfile::tempdir;

fn fixture() -> tempfile::TempDir {
    let d = tempdir().unwrap();
    fs::write(d.path().join("a.rs"), "fn foo() {}\nfn bar() {}\n").unwrap();
    fs::write(d.path().join("b.rs"), "fn foo() {}\n").unwrap();
    fs::write(d.path().join("c.md"), "no rust here\n").unwrap();
    d
}

#[test]
fn files_with_matches_default_mode() {
    let d = fixture();
    let out = grep_v2(GrepArgs {
        pattern: "fn foo".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        glob: None,
        r#type: None,
        output_mode: None,
        head_limit: None,
        before: 0,
        after: 0,
        line_numbers: false,
        multiline: false,
    })
    .unwrap();
    let arr = out["files"].as_array().unwrap();
    assert_eq!(arr.len(), 2);
}

#[test]
fn content_mode_returns_lines() {
    let d = fixture();
    let out = grep_v2(GrepArgs {
        pattern: "fn foo".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        glob: None,
        r#type: None,
        output_mode: Some(OutputMode::Content),
        head_limit: None,
        before: 0,
        after: 0,
        line_numbers: true,
        multiline: false,
    })
    .unwrap();
    let arr = out["matches"].as_array().unwrap();
    assert!(arr.iter().any(|v| v["line"].as_u64() == Some(1)));
}

#[test]
fn count_mode_returns_counts() {
    let d = fixture();
    let out = grep_v2(GrepArgs {
        pattern: "fn".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        glob: None,
        r#type: None,
        output_mode: Some(OutputMode::Count),
        head_limit: None,
        before: 0,
        after: 0,
        line_numbers: false,
        multiline: false,
    })
    .unwrap();
    let arr = out["counts"].as_array().unwrap();
    let total: u64 = arr.iter().map(|v| v["count"].as_u64().unwrap()).sum();
    assert_eq!(total, 3);
}

#[test]
fn head_limit_caps_output() {
    let d = fixture();
    let out = grep_v2(GrepArgs {
        pattern: "fn".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        glob: None,
        r#type: None,
        output_mode: Some(OutputMode::Content),
        head_limit: Some(1),
        before: 0,
        after: 0,
        line_numbers: true,
        multiline: false,
    })
    .unwrap();
    let arr = out["matches"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
}

#[test]
fn type_filter_only_matches_named_type() {
    let d = fixture();
    let out = grep_v2(GrepArgs {
        pattern: "fn".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        glob: None,
        r#type: Some("rust".into()),
        output_mode: None,
        head_limit: None,
        before: 0,
        after: 0,
        line_numbers: false,
        multiline: false,
    })
    .unwrap();
    let arr = out["files"].as_array().unwrap();
    for f in arr {
        assert!(f.as_str().unwrap().ends_with(".rs"));
    }
}

#[test]
fn glob_filter_only_matches_pattern() {
    let d = fixture();
    let out = grep_v2(GrepArgs {
        pattern: "fn".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        glob: Some("*.md".into()),
        r#type: None,
        output_mode: None,
        head_limit: None,
        before: 0,
        after: 0,
        line_numbers: false,
        multiline: false,
    })
    .unwrap();
    let arr = out["files"].as_array().unwrap();
    assert!(arr.is_empty());
}
