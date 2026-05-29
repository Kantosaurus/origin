// SPDX-License-Identifier: Apache-2.0
//! When a memory record is dropped, `MemoryStore` must decrement the refcount
//! of every CAS handle the record referenced so GC can reclaim the shards.
//!
//! Symmetrically, saving a record must increment the refcount so the entry
//! exists in `cas_refs` to begin with.

use origin_cas::{Hash, RefTable, Store as CasStore, StoreConfig};
use origin_mem::quantizer::Quantizer;
use origin_mem::storage::MemoryStore;
use origin_mem::EMBED_DIM;
use origin_store::Store as SqlStore;
use std::sync::Arc;

fn synth(n: usize) -> Vec<[f32; EMBED_DIM]> {
    use rand::{Rng, SeedableRng};
    let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(0x00C0_FFEE);
    (0..n)
        .map(|_| {
            let mut v = [0_f32; EMBED_DIM];
            for s in &mut v {
                *s = rng.gen_range(-1.0..1.0);
            }
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
            for s in &mut v {
                *s /= norm;
            }
            v
        })
        .collect()
}

fn refcount(sql: &SqlStore, h: Hash) -> Option<i64> {
    let table = RefTable::new();
    sql.with_conn(|c| Ok(table.get(c, h).expect("refcount get")))
        .expect("with_conn")
}

#[test]
fn save_increments_then_forget_decrements_refcount() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sql = Arc::new(SqlStore::open(tmp.path().join("origin.db")).expect("sql"));
    let cas = Arc::new(
        CasStore::open(StoreConfig {
            root: tmp.path().join("cas"),
            hot_capacity: 16,
            warm_pack_target_bytes: 64 * 1024,
            cold_zstd_level: 1,
        })
        .expect("cas"),
    );
    let store = MemoryStore::new(Arc::clone(&sql), Arc::clone(&cas));
    let q = Quantizer::fit(&synth(1024), 7).expect("fit");
    store.install_quantizer(&q).expect("install");

    let body = "long body bytes that get CAS'd";
    let v = synth(1)[0];
    let id = store.save(body, &v, &["tag-a"]).expect("save");

    let rec = store.get(id).expect("get").expect("present");
    let handle = Hash::from_bytes(rec.body_handle);

    // After save, the refcount must be 1 — MemoryStore must call RefTable::incr.
    let pre = refcount(&sql, handle);
    assert_eq!(
        pre,
        Some(1),
        "RefTable::incr must run on save_record; got {pre:?}"
    );

    store.forget(id).expect("forget");

    // After forget, the refcount must be 0 — MemoryStore must call RefTable::decr.
    let post = refcount(&sql, handle);
    assert_eq!(post, Some(0), "RefTable::decr must run on forget; got {post:?}");
}
