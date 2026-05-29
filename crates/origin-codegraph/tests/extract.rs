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
