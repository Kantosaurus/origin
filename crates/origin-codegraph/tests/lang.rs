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
    // `cpp` now maps to C++ (see from_extension_maps_new_languages); use a
    // genuinely unknown extension to assert the fall-through arm.
    assert_eq!(Language::from_extension("qzx"), None);
    assert_eq!(Language::from_extension(""), None);
}

#[test]
fn parses_minimal_c_cpp_csharp_ruby_bash() {
    let c = Language::C.parse(b"int main(void){return 0;}").expect("parse c");
    assert_eq!(c.root_node().kind(), "translation_unit");
    let cpp = Language::Cpp
        .parse(b"int main(){return 0;}")
        .expect("parse cpp");
    assert_eq!(cpp.root_node().kind(), "translation_unit");
    let cs = Language::CSharp
        .parse(b"class C { void M() {} }")
        .expect("parse c#");
    assert_eq!(cs.root_node().kind(), "compilation_unit");
    let rb = Language::Ruby.parse(b"def hi\nend\n").expect("parse ruby");
    assert_eq!(rb.root_node().kind(), "program");
    let sh = Language::Bash.parse(b"echo hi\n").expect("parse bash");
    assert_eq!(sh.root_node().kind(), "program");
}

#[test]
fn from_extension_maps_new_languages() {
    assert_eq!(Language::from_extension("c"), Some(Language::C));
    assert_eq!(Language::from_extension("h"), Some(Language::C));
    for ext in ["cpp", "cc", "cxx", "hpp", "hh", "hxx"] {
        assert_eq!(
            Language::from_extension(ext),
            Some(Language::Cpp),
            "ext {ext} should map to C++"
        );
    }
    assert_eq!(Language::from_extension("cs"), Some(Language::CSharp));
    assert_eq!(Language::from_extension("rb"), Some(Language::Ruby));
    assert_eq!(Language::from_extension("sh"), Some(Language::Bash));
    assert_eq!(Language::from_extension("bash"), Some(Language::Bash));
    // Case-insensitive, like the original five.
    assert_eq!(Language::from_extension("CPP"), Some(Language::Cpp));
    assert_eq!(Language::from_extension("RB"), Some(Language::Ruby));
}

#[test]
fn as_discriminant_is_stable_and_appended() {
    // Existing discriminants are a persisted SQL contract and MUST NOT change.
    assert_eq!(Language::Rust.as_discriminant(), 0);
    assert_eq!(Language::TypeScript.as_discriminant(), 1);
    assert_eq!(Language::Python.as_discriminant(), 2);
    assert_eq!(Language::Go.as_discriminant(), 3);
    assert_eq!(Language::Java.as_discriminant(), 4);
    // New languages are appended, never interleaved.
    assert_eq!(Language::C.as_discriminant(), 5);
    assert_eq!(Language::Cpp.as_discriminant(), 6);
    assert_eq!(Language::CSharp.as_discriminant(), 7);
    assert_eq!(Language::Ruby.as_discriminant(), 8);
    assert_eq!(Language::Bash.as_discriminant(), 9);
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
