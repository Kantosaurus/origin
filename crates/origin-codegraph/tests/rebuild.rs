// SPDX-License-Identifier: Apache-2.0
use origin_cas::{Store, StoreConfig};
use origin_codegraph::rebuild::rebuild_paths;
use origin_codegraph::Language;
use std::fs;
use tempfile::tempdir;

fn open_idx(dir: &std::path::Path) -> origin_codegraph::index::CodeGraphIndex {
    let cas = Store::open(StoreConfig {
        root: dir.join("cas"),
        hot_capacity: 64,
        warm_pack_target_bytes: 1 << 16,
        cold_zstd_level: 3,
    })
    .expect("cas");
    let store = origin_store::Store::open(dir.join("s.db")).expect("store");
    origin_codegraph::index::CodeGraphIndex::new(cas, store)
}

#[test]
fn touch_file_triggers_rebuild_report() {
    let dir = tempdir().expect("tempdir");
    let src = dir.path().join("a.rs");
    fs::write(&src, "fn before() {}\n").expect("write a");

    let mut idx = open_idx(dir.path());

    let report1 = rebuild_paths(&mut idx, std::slice::from_ref(&src), Language::Rust).expect("r1");
    assert_eq!(report1.nodes_added + report1.nodes_updated, 1);

    fs::write(&src, "fn before() {}\nfn after() {}\n").expect("rewrite");
    let report2 = rebuild_paths(&mut idx, &[src], Language::Rust).expect("r2");
    assert!(
        report2.nodes_added + report2.nodes_updated >= 1,
        "nothing changed in rebuild2: {report2:?}",
    );
}
