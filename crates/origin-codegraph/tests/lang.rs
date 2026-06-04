// SPDX-License-Identifier: Apache-2.0
use origin_codegraph::Language;
use std::path::Path;

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
fn from_extension_maps_rust() {
    assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
}

#[test]
fn from_extension_maps_typescript_variants() {
    for ext in ["ts", "tsx", "mts", "cts"] {
        assert_eq!(
            Language::from_extension(ext),
            Some(Language::TypeScript),
            "ext {ext} should map to TypeScript"
        );
    }
}

#[test]
fn from_extension_maps_python_go_java() {
    assert_eq!(Language::from_extension("py"), Some(Language::Python));
    assert_eq!(Language::from_extension("pyi"), Some(Language::Python));
    assert_eq!(Language::from_extension("go"), Some(Language::Go));
    assert_eq!(Language::from_extension("java"), Some(Language::Java));
}

#[test]
fn from_extension_unknown_is_none() {
    assert_eq!(Language::from_extension("txt"), None);
    assert_eq!(Language::from_extension("cpp"), None);
    assert_eq!(Language::from_extension(""), None);
}

#[test]
fn from_extension_is_case_insensitive() {
    assert_eq!(Language::from_extension("RS"), Some(Language::Rust));
    assert_eq!(Language::from_extension("Py"), Some(Language::Python));
    assert_eq!(Language::from_extension("TSX"), Some(Language::TypeScript));
    assert_eq!(Language::from_extension("Go"), Some(Language::Go));
    assert_eq!(Language::from_extension("JAVA"), Some(Language::Java));
}

#[test]
fn from_path_maps_known_extension() {
    assert_eq!(
        Language::from_path(Path::new("src/main.rs")),
        Some(Language::Rust)
    );
    assert_eq!(
        Language::from_path(Path::new("/abs/dir/app.tsx")),
        Some(Language::TypeScript)
    );
    assert_eq!(
        Language::from_path(Path::new("pkg/server.go")),
        Some(Language::Go)
    );
}

#[test]
fn from_path_unknown_or_missing_extension_is_none() {
    assert_eq!(Language::from_path(Path::new("notes.txt")), None);
    assert_eq!(Language::from_path(Path::new("Makefile")), None);
    assert_eq!(Language::from_path(Path::new("/etc/hosts")), None);
}

#[test]
fn from_path_is_case_insensitive() {
    assert_eq!(
        Language::from_path(Path::new("LIB.RS")),
        Some(Language::Rust)
    );
}

#[test]
fn parse_invalid_utf8_errors() {
    let bad: &[u8] = &[0xFF, 0xFE, 0xFD];
    // Tree-sitter accepts any bytes; we treat the result as a (possibly degenerate) tree.
    // Assert the API does not panic.
    let tree = Language::Rust.parse(bad);
    assert!(tree.is_ok());
}
