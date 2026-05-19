use origin_cas::{Store, StoreConfig};
use origin_tools::builtins::recall::{recall_tool, Region};
use std::sync::Arc;
use tempfile::tempdir;

#[test]
fn recalls_line_range_from_handle() {
    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        Store::open(StoreConfig {
            root: dir.path().to_path_buf(),
            hot_capacity: 64,
            warm_pack_target_bytes: 64 * 1024,
            cold_zstd_level: 3,
        })
        .expect("open"),
    );
    let body = (1..=30)
        .map(|n| format!("line-{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let h = store.put(body.as_bytes()).expect("put");

    let region = Region::Lines { start: 10, end: 12 };
    let out = recall_tool(&store, *h.as_bytes(), Some(region)).expect("ok");
    assert_eq!(out, "line-10\nline-11\nline-12");
}

#[test]
fn recall_match_returns_matching_lines() {
    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        Store::open(StoreConfig {
            root: dir.path().to_path_buf(),
            hot_capacity: 64,
            warm_pack_target_bytes: 64 * 1024,
            cold_zstd_level: 3,
        })
        .expect("open"),
    );
    let body = "alpha\nBETA\ngamma\nbeta-2";
    let h = store.put(body.as_bytes()).expect("put");
    let out = recall_tool(
        &store,
        *h.as_bytes(),
        Some(Region::Match {
            pattern: "(?i)beta".into(),
        }),
    )
    .expect("ok");
    assert_eq!(out, "BETA\nbeta-2");
}

#[test]
fn recall_with_none_region_returns_full_body() {
    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        Store::open(StoreConfig {
            root: dir.path().to_path_buf(),
            hot_capacity: 64,
            warm_pack_target_bytes: 64 * 1024,
            cold_zstd_level: 3,
        })
        .expect("open"),
    );
    let body = "hello world";
    let h = store.put(body.as_bytes()).expect("put");
    let out = recall_tool(&store, *h.as_bytes(), None).expect("ok");
    assert_eq!(out, "hello world");
}
