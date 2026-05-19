use origin_mem::index::MemIndex;
use origin_mem::injector::Injector;
use origin_mem::storage::MemoryStore;
use parking_lot::RwLock;
use std::sync::Arc;

#[test]
fn returns_none_when_below_threshold() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sql = Arc::new(origin_store::Store::open(tmp.path().join("o.db")).expect("sql"));
    let cas = Arc::new(
        origin_cas::Store::open(origin_cas::StoreConfig {
            root: tmp.path().join("cas"),
            hot_capacity: 16,
            warm_pack_target_bytes: 64 * 1024,
            cold_zstd_level: 1,
        })
        .expect("cas"),
    );
    let store = Arc::new(MemoryStore::new(Arc::clone(&sql), Arc::clone(&cas)));
    let embedder = Arc::new(
        origin_mem::Embedder::from_path(std::path::Path::new("tests/fixtures/stub_minilm.onnx"))
            .expect("embedder"),
    );
    let index = Arc::new(RwLock::new(MemIndex::new()));
    let injector = Injector::new(embedder, index, store);
    let out = injector.for_prompt("anything", 5).expect("injector");
    assert!(out.is_none());
}
