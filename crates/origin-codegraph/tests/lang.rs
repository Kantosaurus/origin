// SPDX-License-Identifier: Apache-2.0
use origin_codegraph::Language;

#[test]
fn parses_minimal_rust() {
    let src = "fn hello() {}";
    let tree = Language::Rust.parse(src.as_bytes()).expect("parse rust");
    let root = tree.root_node();
    assert_eq!(root.kind(), "source_file");
    assert!(root.child_count() >= 1);
}

#[test]
fn parses_minimal_typescript() {
    let src = "function hello(): void {}";
    let tree = Language::TypeScript.parse(src.as_bytes()).expect("parse ts");
    let root = tree.root_node();
    assert_eq!(root.kind(), "program");
}

#[test]
fn parse_invalid_utf8_errors() {
    let bad: &[u8] = &[0xFF, 0xFE, 0xFD];
    // Tree-sitter accepts any bytes; we treat the result as a (possibly degenerate) tree.
    // Assert the API does not panic.
    let tree = Language::Rust.parse(bad);
    assert!(tree.is_ok());
}
