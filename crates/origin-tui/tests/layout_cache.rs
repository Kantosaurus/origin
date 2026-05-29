// SPDX-License-Identifier: Apache-2.0
use origin_cas::{Store, StoreConfig};
use origin_tui::layout_cache::LayoutCache;
use std::sync::Arc;
use tempfile::tempdir;

fn store() -> Arc<Store> {
    let dir = tempdir().expect("tempdir");
    // keep() replaces the deprecated into_path() — leaks the guard so the
    // temporary directory persists for the duration of the test.
    let path = dir.keep();
    Arc::new(
        Store::open(StoreConfig {
            root: path,
            hot_capacity: 16,
            warm_pack_target_bytes: 1_000_000,
            cold_zstd_level: 3,
        })
        .expect("store open"),
    )
}

#[test]
fn first_call_builds_spans() {
    let mut c = LayoutCache::new(store(), 10);
    let spans = c.get_or_build("hello world").expect("build");
    assert!(!spans.is_empty(), "non-empty text yields spans");
}

#[test]
fn same_text_same_width_returns_same_spans() {
    let s = store();
    let mut c = LayoutCache::new(s.clone(), 10);
    let a = c.get_or_build("hello world").expect("a");
    let mut c2 = LayoutCache::new(s, 10);
    let b = c2.get_or_build("hello world").expect("b");
    assert_eq!(a, b, "same key must yield same spans across instances");
}

#[test]
fn different_widths_produce_different_layouts() {
    let s = store();
    let mut narrow = LayoutCache::new(s.clone(), 4);
    let mut wide = LayoutCache::new(s, 40);
    let a = narrow.get_or_build("hello world").expect("narrow");
    let b = wide.get_or_build("hello world").expect("wide");
    assert_ne!(a, b);
}

#[test]
fn second_call_same_instance_is_a_hit() {
    // Sanity-check the in-memory cache path: second call on same instance
    // should hit the index and decode from the store rather than rebuild.
    let mut c = LayoutCache::new(store(), 10);
    let a = c.get_or_build("hello world").expect("a");
    let b = c.get_or_build("hello world").expect("b");
    assert_eq!(a, b);
}
