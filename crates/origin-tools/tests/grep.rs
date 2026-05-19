use origin_tools::builtins::grep_tool::grep_tool;
use std::fs;

#[test]
fn finds_pattern_in_one_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.txt"), "alpha\nbeta\ngamma").expect("write a.txt");
    fs::write(dir.path().join("b.txt"), "delta\nepsilon").expect("write b.txt");

    let mut hits = grep_tool(r"alpha|epsilon", dir.path().to_str().expect("utf8 path")).expect("grep ok");
    hits.sort();
    assert_eq!(hits.len(), 2, "expected one hit per file, got {hits:?}");
}

#[test]
fn returns_empty_when_no_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.txt"), "alpha\nbeta").expect("write");
    let hits = grep_tool("zzz", dir.path().to_str().expect("utf8")).expect("grep ok");
    assert!(hits.is_empty(), "expected no hits, got {hits:?}");
}

#[test]
fn invalid_regex_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = grep_tool("(", dir.path().to_str().expect("utf8")).expect_err("bad regex");
    assert!(err.contains("regex"), "expected regex error, got: {err}");
}
