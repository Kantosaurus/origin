use origin_cas::{Hash, Store, StoreConfig};
use tempfile::tempdir;

fn cfg(dir: &std::path::Path) -> StoreConfig {
    StoreConfig {
        root: dir.to_path_buf(),
        hot_capacity: 4,
        warm_pack_target_bytes: 1024 * 1024,
        cold_zstd_level: 3,
    }
}

#[test]
fn put_then_get_via_hot() {
    let dir = tempdir().expect("tempdir");
    let store = Store::open(cfg(dir.path())).expect("open");

    let h = store.put(b"hello").expect("put");
    let v = store.get(h).expect("get").expect("present");
    assert_eq!(v.as_slice(), b"hello");
    assert_eq!(h, Hash::of(b"hello"));
}

#[test]
fn dedup_returns_same_hash() {
    let dir = tempdir().expect("tempdir");
    let store = Store::open(cfg(dir.path())).expect("open");

    let a = store.put(b"dup").expect("put a");
    let b = store.put(b"dup").expect("put b");
    assert_eq!(a, b);
}

#[test]
fn eviction_from_hot_falls_back_to_warm() {
    let dir = tempdir().expect("tempdir");
    let store = Store::open(cfg(dir.path())).expect("open");

    let h0 = store.put(b"first").expect("put");
    for i in 0..6 {
        let _ = store.put(format!("filler-{i}").as_bytes()).expect("put");
    }
    let v = store.get(h0).expect("get").expect("present");
    assert_eq!(v.as_slice(), b"first");
}

#[test]
fn explicit_demote_to_cold_still_readable() {
    let dir = tempdir().expect("tempdir");
    let store = Store::open(cfg(dir.path())).expect("open");

    let h = store.put(b"please-compress-me").expect("put");
    store.demote_to_cold(h).expect("demote");
    let v = store.get(h).expect("get").expect("present");
    assert_eq!(v.as_slice(), b"please-compress-me");
}

proptest::proptest! {
    #[test]
    fn random_read_write_evict_preserves_content(items in proptest::collection::vec(proptest::collection::vec(proptest::num::u8::ANY, 0..512), 1..50)) {
        let dir = tempdir().expect("tempdir");
        let store = Store::open(cfg(dir.path())).expect("open");

        let mut handles = Vec::new();
        for item in &items {
            let h = store.put(item).expect("put");
            handles.push(h);
        }
        for (h, item) in handles.iter().zip(&items) {
            let v = store.get(*h).expect("get").expect("present");
            proptest::prop_assert_eq!(v.as_slice(), &item[..]);
        }
    }
}
