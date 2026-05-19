use origin_codegraph::{extract::extract_nodes, Language, NodeKind};

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
