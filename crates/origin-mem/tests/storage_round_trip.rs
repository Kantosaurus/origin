// SPDX-License-Identifier: Apache-2.0
use origin_mem::quantizer::Quantizer;
use origin_mem::storage::{EdgeKind, MemoryStore};
use origin_mem::EMBED_DIM;
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

#[test]
fn save_then_get_round_trips() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sql = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("sql"));
    let cas = Arc::new(
        origin_cas::Store::open(origin_cas::StoreConfig {
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
    let body = "user prefers terse responses with no trailing summaries";
    let v = synth(1)[0];
    let id = store.save(body, &v, &["feedback", "terseness"]).expect("save");
    let got = store.get(id).expect("get").expect("present");
    assert!(got.body_preview.len() <= 64);
    assert!(got.tags.iter().any(|t| t == "feedback"));
    let body_bytes = cas
        .get(origin_cas::Hash::from_bytes(got.body_handle))
        .expect("cas get")
        .expect("present");
    assert_eq!(std::str::from_utf8(&body_bytes).expect("utf8"), body);
}

#[test]
fn edge_and_supersede_round_trip() {
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
    let store = MemoryStore::new(sql, cas);
    let q = Quantizer::fit(&synth(1024), 8).expect("fit");
    store.install_quantizer(&q).expect("install");
    let v = synth(1)[0];
    let loser = store.save("old", &v, &[]).expect("save");
    let winner = store.save("new", &v, &[]).expect("save");
    store.mark_superseded(loser, winner).expect("supersede");
    store
        .add_edge(loser, winner, EdgeKind::Supersedes, 1.0)
        .expect("edge");
    let l = store.get(loser).expect("get").expect("present");
    assert_eq!(l.superseded_by, Some(winner));
}
