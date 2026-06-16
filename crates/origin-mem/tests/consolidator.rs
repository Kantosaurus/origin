// SPDX-License-Identifier: Apache-2.0
use origin_mem::consolidator::Consolidator;
use origin_mem::index::MemIndex;
use origin_mem::memory_id_to_u64;
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

/// Build a degenerate-but-valid quantizer directly from its binary format,
/// bypassing the slow k-means fit. All centroids are `unit_vec(0.0)`; the
/// encode/decode path round-trips correctly for this test.
fn degenerate_quantizer() -> Quantizer {
    let magic: u32 = 0xC0FF_EE42;
    let version: u32 = 1;
    let scale: f32 = 1.0;
    let centroid = unit_vec(0.0);
    let mut buf = Vec::with_capacity(12 + origin_mem::quantizer::NUM_CENTROIDS * EMBED_DIM * 4);
    buf.extend_from_slice(&magic.to_le_bytes());
    buf.extend_from_slice(&version.to_le_bytes());
    buf.extend_from_slice(&scale.to_le_bytes());
    for _ in 0..origin_mem::quantizer::NUM_CENTROIDS {
        for v in &centroid {
            buf.extend_from_slice(&v.to_le_bytes());
        }
    }
    Quantizer::from_bytes(&buf).expect("build degenerate quantizer")
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
    store.install_quantizer(&degenerate_quantizer()).expect("install");
    // Stamp explicit, strictly-distinct creation timestamps rather than racing
    // the platform timer with a `sleep` — a 2ms gap collides inside Windows'
    // ~15ms timer resolution, flipping the older/newer ordering the supersede
    // proposal depends on. `save_at` makes `id_b` provably newer than `id_a`.
    let t0: i64 = 1_700_000_000_000;
    let id_a = store
        .save_at("user is a rust engineer", &unit_vec(0.0), &[], t0)
        .expect("save");
    let id_b = store
        .save_at(
            "user is a senior rust engineer",
            &unit_vec(0.001),
            &[],
            t0 + 1_000,
        )
        .expect("save");

    // Populate the HNSW index. The injector daemon ordinarily owns this, but
    // for the unit test we drive it directly. CRITICAL: the index MUST be keyed
    // by the same stable id the consolidator now uses — `memory_id_to_u64(&id)`
    // — exactly as the daemon's injector/memory_wiring populate it. Keying it
    // positionally (0, 1) would no longer be resolvable by the consolidator.
    let mut idx = MemIndex::new();
    idx.insert(memory_id_to_u64(&id_a), &unit_vec(0.0)).expect("ins");
    idx.insert(memory_id_to_u64(&id_b), &unit_vec(0.001))
        .expect("ins");
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

/// Regression for the id-scheme mismatch: the consolidator must identify
/// memories by the STABLE id `memory_id_to_u64(&id)`, not by the positional
/// `iter_all` index. We construct a case where those two schemes diverge (the
/// stable u64 of every real ULID is a large millisecond-timestamp number, never
/// 0/1/2…), populate the index with the stable scheme (as the daemon does), and
/// assert the consolidator still resolves the candidate and references the
/// correct row by its real `MemoryId`. Under the old positional scheme the
/// index keys (large u64s) would miss `Vec::get(id as usize)` entirely and the
/// report would be empty.
#[test]
fn consolidator_uses_stable_memory_id_not_positional_index() {
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
    store.install_quantizer(&degenerate_quantizer()).expect("install");

    // Explicit strictly-distinct timestamps (see sibling test): avoids the
    // same-millisecond collision a `sleep`-based gap suffers under Windows'
    // coarse timer resolution, which would otherwise flip loser/winner.
    let t0: i64 = 1_700_000_000_000;
    let id_a = store
        .save_at("user is a rust engineer", &unit_vec(0.0), &[], t0)
        .expect("save");
    let id_b = store
        .save_at(
            "user is a senior rust engineer",
            &unit_vec(0.001),
            &[],
            t0 + 1_000,
        )
        .expect("save");

    // Sanity: the stable ids are large (timestamp-derived) values, provably
    // unequal to the positional indices 0/1 the old code assumed. This is what
    // makes the two id schemes mutually exclusive.
    let uid_a = memory_id_to_u64(&id_a);
    let uid_b = memory_id_to_u64(&id_b);
    assert!(
        uid_a > 1 && uid_b > 1,
        "stable ids must not collide with positional 0/1"
    );
    assert_ne!(uid_a, uid_b, "the two memories must have distinct stable ids");

    // Populate the index the way the daemon does: keyed by the stable id.
    let mut idx = MemIndex::new();
    idx.insert(uid_a, &unit_vec(0.0)).expect("ins");
    idx.insert(uid_b, &unit_vec(0.001)).expect("ins");
    let index = Arc::new(RwLock::new(idx));
    let cons = Consolidator::new(Arc::clone(&store), index);

    let report = cons.run_pass(32).expect("pass");

    // With the stable-id scheme wired through, the consolidator resolves the
    // candidate and proposes the supersede with the CORRECT row identities.
    assert!(
        !report.supersedes_proposed.is_empty(),
        "stable-id consolidator must resolve candidates and propose a supersede"
    );
    let (loser, winner) = report.supersedes_proposed[0];
    assert_eq!(loser, id_a, "older row (by real MemoryId) is the loser");
    assert_eq!(winner, id_b, "newer row (by real MemoryId) is the winner");
}
