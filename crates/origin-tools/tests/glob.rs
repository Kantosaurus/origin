use origin_tools::builtins::glob_tool::glob_tool;
use std::fs::{self, File};

fn make_tree(root: &std::path::Path) {
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::create_dir_all(root.join("tests")).expect("mkdir tests");
    File::create(root.join("src/main.rs")).expect("create main.rs");
    File::create(root.join("src/lib.rs")).expect("create lib.rs");
    File::create(root.join("tests/it.rs")).expect("create it.rs");
    File::create(root.join("README.md")).expect("create README.md");
}

#[test]
fn matches_rust_files_under_src() {
    let dir = tempfile::tempdir().expect("tempdir");
    make_tree(dir.path());
    let pattern = dir.path().join("src/*.rs");
    let mut hits = glob_tool(pattern.to_str().expect("utf8")).expect("glob ok");
    hits.sort();
    assert_eq!(
        hits.len(),
        2,
        "expected exactly 2 rust files in src/, got {hits:?}"
    );
    assert!(hits[0].ends_with("lib.rs"));
    assert!(hits[1].ends_with("main.rs"));
}

#[test]
fn returns_empty_when_no_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    make_tree(dir.path());
    let pattern = dir.path().join("**/*.py");
    let hits = glob_tool(pattern.to_str().expect("utf8")).expect("glob ok");
    assert!(hits.is_empty(), "expected no python hits, got {hits:?}");
}

#[test]
fn recursive_pattern_descends() {
    let dir = tempfile::tempdir().expect("tempdir");
    make_tree(dir.path());
    let pattern = dir.path().join("**/*.rs");
    let hits = glob_tool(pattern.to_str().expect("utf8")).expect("glob ok");
    assert_eq!(hits.len(), 3, "expected 3 rust files anywhere, got {hits:?}");
}
