use origin_cas::{Store, StoreConfig};
use origin_tools::builtins::recall::{recall_tool, Region};
use std::sync::Arc;
use tempfile::tempdir;

fn open_store() -> Arc<Store> {
    let dir = tempdir().expect("tempdir");
    // Leak the tempdir so the store outlives this fn for the test's lifetime.
    let path = dir.keep();
    Arc::new(
        Store::open(StoreConfig {
            root: path,
            hot_capacity: 64,
            warm_pack_target_bytes: 64 * 1024,
            cold_zstd_level: 3,
        })
        .expect("open"),
    )
}

#[test]
fn outline_only_extracts_rust_signatures() {
    let store = open_store();
    let body = "// preamble\nfn foo() {\n    let x = 1;\n}\n\npub struct Bar {\n    field: u32,\n}\nplain text line\n";
    let h = store.put(body.as_bytes()).expect("put");
    let out = recall_tool(&store, *h.as_bytes(), Some(Region::OutlineOnly)).expect("ok");
    assert_eq!(out, "fn foo() {\npub struct Bar {");
}

#[test]
fn outline_only_extracts_markdown_headings() {
    let store = open_store();
    let body = "# Title\nsome prose here\n## Section\nmore prose\n";
    let h = store.put(body.as_bytes()).expect("put");
    let out = recall_tool(&store, *h.as_bytes(), Some(Region::OutlineOnly)).expect("ok");
    assert_eq!(out, "# Title\n## Section");
}

#[test]
fn outline_only_empty_when_no_structure() {
    let store = open_store();
    let body = "just some plain prose\nwith no structure at all\nnothing to see here\n";
    let h = store.put(body.as_bytes()).expect("put");
    let out = recall_tool(&store, *h.as_bytes(), Some(Region::OutlineOnly)).expect("ok");
    assert_eq!(out, "<no outline structure detected>");
}

#[test]
fn outline_only_caps_at_200_entries() {
    let store = open_store();
    let mut body = String::new();
    for n in 0..250 {
        body.push_str(&format!("fn x_{n}() {{}}\n"));
    }
    let h = store.put(body.as_bytes()).expect("put");
    let out = recall_tool(&store, *h.as_bytes(), Some(Region::OutlineOnly)).expect("ok");
    assert_eq!(out.split('\n').count(), 200);
}
