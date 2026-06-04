// SPDX-License-Identifier: Apache-2.0
use origin_cas::{Store as Cas, StoreConfig};
use origin_codegraph::{
    extract::{extract_nodes, extract_nodes_with_cas},
    Language, NodeKind,
};
use tempfile::tempdir;

#[test]
fn extracts_rust_functions() {
    let src = r"
fn alpha() {}
fn beta(x: u32) -> u32 { x + 1 }
struct Gamma;
";
    let nodes = extract_nodes(Language::Rust, src.as_bytes()).expect("extract");
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta"));
    assert!(names.contains(&"Gamma"));

    let beta = nodes.iter().find(|n| n.name == "beta").expect("beta node");
    assert_eq!(beta.kind, NodeKind::Function);
    assert!(beta.range.end > beta.range.start);
}

#[test]
fn extract_with_cas_yields_nonzero_signature_handle() {
    let src = r"
fn alpha() { let _x = 1; }
struct Gamma;
";
    let dir = tempdir().expect("tempdir");
    let cas = Cas::open(StoreConfig {
        root: dir.path().join("cas"),
        hot_capacity: 64,
        warm_pack_target_bytes: 1 << 16,
        cold_zstd_level: 3,
    })
    .expect("cas");
    let nodes = extract_nodes_with_cas(Language::Rust, src.as_bytes(), &cas).expect("extract");
    let alpha = nodes.iter().find(|n| n.name == "alpha").expect("alpha");
    assert_eq!(alpha.kind, NodeKind::Function);
    assert_ne!(
        alpha.signature_handle, [0u8; 32],
        "function signature handle must be CAS-populated"
    );
    assert_ne!(
        alpha.body_handle, [0u8; 32],
        "function body handle must be CAS-populated"
    );
    assert!(alpha.body_range.is_some(), "function must have a body span");

    // Unit struct has no body block — handle stays zero by design.
    let gamma = nodes.iter().find(|n| n.name == "Gamma").expect("gamma");
    assert_ne!(
        gamma.signature_handle, [0u8; 32],
        "struct signature handle is the whole declaration"
    );
    assert_eq!(gamma.body_handle, [0u8; 32], "unit struct has no body");
}

#[test]
fn extracts_typescript_functions() {
    let src = r"
function alpha(): void {}
function beta(x: number): number { return x + 1; }
class Gamma {}
";
    let nodes = extract_nodes(Language::TypeScript, src.as_bytes()).expect("extract");
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta"));
    assert!(names.contains(&"Gamma"));
}

// --- Curated grammar additions (C / C++ / C# / Ruby / Bash) ---

fn names_of(lang: Language, src: &[u8]) -> Vec<String> {
    extract_nodes(lang, src)
        .expect("extract")
        .into_iter()
        .map(|n| n.name)
        .collect()
}

#[test]
fn extracts_c_functions_and_structs() {
    // C `function_definition` has no `name` field (only a `declarator` chain),
    // so this exercises the declarator-walk fallback in `classify`.
    let names = names_of(
        Language::C,
        b"int alpha(void) { return 0; }\nstruct Gamma { int x; };\n",
    );
    assert!(names.iter().any(|n| n == "alpha"), "names={names:?}");
    assert!(names.iter().any(|n| n == "Gamma"), "names={names:?}");
}

#[test]
fn extracts_cpp_class_and_function() {
    let names = names_of(
        Language::Cpp,
        b"namespace ns { class Widget { public: void run() {} }; int beta(int x) { return x + 1; } }\n",
    );
    assert!(names.iter().any(|n| n == "Widget"), "names={names:?}");
    assert!(names.iter().any(|n| n == "beta"), "names={names:?}");
    assert!(names.iter().any(|n| n == "ns"), "names={names:?}");
}

#[test]
fn extracts_csharp_class_and_method() {
    let names = names_of(
        Language::CSharp,
        b"namespace N { class Foo { public int Bar() { return 1; } } }",
    );
    assert!(names.iter().any(|n| n == "Foo"), "names={names:?}");
    assert!(names.iter().any(|n| n == "Bar"), "names={names:?}");
    assert!(names.iter().any(|n| n == "N"), "names={names:?}");
}

#[test]
fn extracts_ruby_class_method_module() {
    let names = names_of(
        Language::Ruby,
        b"module M\n  class Foo\n    def bar\n    end\n  end\nend\n",
    );
    assert!(names.iter().any(|n| n == "M"), "names={names:?}");
    assert!(names.iter().any(|n| n == "Foo"), "names={names:?}");
    assert!(names.iter().any(|n| n == "bar"), "names={names:?}");
}

#[test]
fn extracts_bash_function() {
    let names = names_of(
        Language::Bash,
        b"alpha() {\n  echo hi\n}\nfunction beta {\n  echo yo\n}\n",
    );
    assert!(names.iter().any(|n| n == "alpha"), "names={names:?}");
    assert!(names.iter().any(|n| n == "beta"), "names={names:?}");
}
// --- Extended-grammar coverage (codegraph parity with repomap scanner) ---
//
// Each test parses a tiny snippet in a language whose grammar was added beyond
// the original 5/10, and asserts that at least one definition node is recovered
// with the expected name and kind.

fn names_in(nodes: &[origin_codegraph::CodeNode]) -> Vec<&str> {
    nodes.iter().map(|n| n.name.as_str()).collect()
}

#[test]
fn extracts_php_definitions() {
    let src = r"<?php
function alpha() { return 1; }
class Gamma {
    function method_one() { return 2; }
}
";
    let nodes = extract_nodes(Language::Php, src.as_bytes()).expect("extract php");
    let names = names_in(&nodes);
    assert!(names.contains(&"alpha"), "php function: {names:?}");
    assert!(names.contains(&"Gamma"), "php class: {names:?}");
    let alpha = nodes.iter().find(|n| n.name == "alpha").expect("alpha");
    assert_eq!(alpha.kind, NodeKind::Function);
    let gamma = nodes.iter().find(|n| n.name == "Gamma").expect("Gamma");
    assert_eq!(gamma.kind, NodeKind::Class);
}

#[test]
fn extracts_swift_definitions() {
    let src = r"
func alpha() -> Int { return 1 }
class Gamma {}
struct Delta {}
";
    let nodes = extract_nodes(Language::Swift, src.as_bytes()).expect("extract swift");
    let names = names_in(&nodes);
    assert!(names.contains(&"alpha"), "swift func: {names:?}");
    assert!(names.contains(&"Gamma"), "swift class: {names:?}");
    let alpha = nodes.iter().find(|n| n.name == "alpha").expect("alpha");
    assert_eq!(alpha.kind, NodeKind::Function);
}

#[test]
fn extracts_kotlin_definitions() {
    let src = r"
fun alpha(): Int { return 1 }
class Gamma {}
";
    let nodes = extract_nodes(Language::Kotlin, src.as_bytes()).expect("extract kotlin");
    let names = names_in(&nodes);
    assert!(names.contains(&"alpha"), "kotlin fun: {names:?}");
    assert!(names.contains(&"Gamma"), "kotlin class: {names:?}");
    let alpha = nodes.iter().find(|n| n.name == "alpha").expect("alpha");
    assert_eq!(alpha.kind, NodeKind::Function);
}

#[test]
fn extracts_scala_definitions() {
    let src = r"
def alpha(): Int = 1
class Gamma {}
object Delta {}
";
    let nodes = extract_nodes(Language::Scala, src.as_bytes()).expect("extract scala");
    let names = names_in(&nodes);
    assert!(names.contains(&"alpha"), "scala def: {names:?}");
    assert!(names.contains(&"Gamma"), "scala class: {names:?}");
    let alpha = nodes.iter().find(|n| n.name == "alpha").expect("alpha");
    assert_eq!(alpha.kind, NodeKind::Function);
}

#[test]
fn extracts_haskell_definitions() {
    let src = "alpha :: Int\nalpha = 1\n\ndata Gamma = Gamma\n";
    let nodes = extract_nodes(Language::Haskell, src.as_bytes()).expect("extract haskell");
    let names = names_in(&nodes);
    assert!(
        names.iter().any(|n| *n == "alpha" || *n == "Gamma"),
        "haskell def: {names:?}"
    );
}

#[test]
fn extracts_lua_definitions() {
    let src = r"
function alpha()
  return 1
end
local function beta()
  return 2
end
";
    let nodes = extract_nodes(Language::Lua, src.as_bytes()).expect("extract lua");
    let names = names_in(&nodes);
    assert!(names.contains(&"alpha"), "lua function: {names:?}");
    let alpha = nodes.iter().find(|n| n.name == "alpha").expect("alpha");
    assert_eq!(alpha.kind, NodeKind::Function);
}

#[test]
fn extracts_elixir_definitions() {
    let src = r"
defmodule Gamma do
  def alpha do
    1
  end
end
";
    let nodes = extract_nodes(Language::Elixir, src.as_bytes()).expect("extract elixir");
    let names = names_in(&nodes);
    assert!(
        names.iter().any(|n| *n == "alpha" || *n == "Gamma"),
        "elixir def: {names:?}"
    );
}
