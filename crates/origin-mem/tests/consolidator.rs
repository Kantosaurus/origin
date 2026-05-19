use origin_mem::consolidator::Consolidator;
use origin_mem::index::MemIndex;
use origin_mem::quantizer::Quantizer;
use origin_mem::storage::MemoryStore;
use origin_mem::EMBED_DIM;
use parking_lot::RwLock;
use std::sync::Arc;

fn unit_vec(seed: f32) -> [f32; EMBED_DIM] {
    let mut v = [0_f32; EMBED_DIM];
    v[0] = seed.cos();
    v[1] = seed.sin();
    v
}

#[test]
fn near_duplicates_get_supersede_proposal() {
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
    let q = Quantizer::fit(&vec![unit_vec(0.0); 1024], 9).expect("fit");
    store.install_quantizer(&q).expect("install");
    let id_a = store
        .save("user is a rust engineer", &unit_vec(0.0), &[])
        .expect("save");
    std::thread::sleep(std::time::Duration::from_millis(2));
    let id_b = store
        .save("user is a senior rust engineer", &unit_vec(0.001), &[])
        .expect("save");

    // Populate the HNSW index. The injector daemon ordinarily owns this, but
    // for the unit test we drive it directly using a side-table id->index map.
    let mut idx = MemIndex::new();
    // P6.3 index keys are u64. We need a stable u64-side that matches a lookup
    // closure used internally by the consolidator. The consolidator's
    // store->index id mapping is described below — adapt your implementation
    // accordingly.
    // Simplest: map each ULID to a sequential u64 by insertion order. The
    // Consolidator can rebuild this mapping from `store.iter_all()` order.
    idx.insert(0, &unit_vec(0.0)).expect("ins");
    idx.insert(1, &unit_vec(0.001)).expect("ins");
    let index = Arc::new(RwLock::new(idx));
    let cons = Consolidator::new(Arc::clone(&store), Arc::clone(&index));

    let report = cons.run_pass(32).expect("pass");
    assert!(
        !report.supersedes_proposed.is_empty(),
        "should propose at least one supersede"
    );
    let (loser, winner) = report.supersedes_proposed[0];
    assert_eq!(loser, id_a, "older should be the loser");
    assert_eq!(winner, id_b);
}
