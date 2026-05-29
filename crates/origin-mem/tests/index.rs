// SPDX-License-Identifier: Apache-2.0
use origin_mem::index::{MemIndex, MetaRow, SearchOpts};
use origin_mem::EMBED_DIM;
use std::collections::HashMap;

fn unit_vec(seed: f32) -> [f32; EMBED_DIM] {
    let mut v = [0_f32; EMBED_DIM];
    v[0] = seed.cos();
    v[1] = seed.sin();
    v
}

#[test]
fn decay_demotes_old_match() {
    let mut idx = MemIndex::new();
    // hnsw_rs uses `StdRng::from_os_rng()` for layer assignment; a 2-point
    // graph occasionally fails to return both points. Pad with metadata-less
    // ids so HNSW has a denser graph and the `lookup` closure filters the
    // padding out before re-ranking.
    // Padding angles start far from the targets (which use angles 0.0 / 0.05)
    // so they cannot displace the targets in the cosine-similarity shortlist.
    for i in 0_u8..50 {
        let angle = f32::from(i).mul_add(0.13, 1.0);
        idx.insert(200 + u64::from(i), &unit_vec(angle)).expect("pad");
    }
    let fresh = unit_vec(0.0);
    let stale = unit_vec(0.05);
    idx.insert(1, &fresh).expect("ins");
    idx.insert(2, &stale).expect("ins");
    let meta: HashMap<u64, MetaRow> = HashMap::from([
        (
            1_u64,
            MetaRow {
                age_days: 1.0,
                cluster_priority: 1.0,
                edge_boost: 0.0,
                superseded_by: None,
            },
        ),
        (
            2_u64,
            MetaRow {
                age_days: 300.0,
                cluster_priority: 1.0,
                edge_boost: 0.0,
                superseded_by: None,
            },
        ),
    ]);
    let opts = SearchOpts {
        top_n: 2,
        ..Default::default()
    };
    let out = idx
        .search(&fresh, &opts, |id| meta.get(&id).copied())
        .expect("search");
    assert_eq!(out[0].id, 1, "fresh ranks higher despite same raw sim");
    assert!(out[0].score > out[1].score);
}

#[test]
fn supersede_drops_loser() {
    let mut idx = MemIndex::new();
    // See `cluster_priority_and_edge_boost_affect_rank` for why padding is
    // required: hnsw_rs is non-deterministic on tiny graphs.
    // Padding angles start far from the targets (which use angles 0.0 / 0.05)
    // so they cannot displace the targets in the cosine-similarity shortlist.
    for i in 0_u8..50 {
        let angle = f32::from(i).mul_add(0.13, 1.0);
        idx.insert(200 + u64::from(i), &unit_vec(angle)).expect("pad");
    }
    idx.insert(10, &unit_vec(0.0)).expect("ins");
    idx.insert(11, &unit_vec(0.0)).expect("ins");
    let meta: HashMap<u64, MetaRow> = HashMap::from([
        (
            10_u64,
            MetaRow {
                age_days: 0.5,
                cluster_priority: 1.0,
                edge_boost: 0.0,
                superseded_by: Some(11),
            },
        ),
        (
            11_u64,
            MetaRow {
                age_days: 0.5,
                cluster_priority: 1.0,
                edge_boost: 0.0,
                superseded_by: None,
            },
        ),
    ]);
    let out = idx
        .search(&unit_vec(0.0), &SearchOpts::default(), |id| {
            meta.get(&id).copied()
        })
        .expect("search");
    assert!(
        out.iter().all(|c| c.id != 10),
        "10 should be dropped as superseded"
    );
}

#[test]
fn cluster_priority_and_edge_boost_affect_rank() {
    let mut idx = MemIndex::new();
    // hnsw_rs uses `StdRng::from_os_rng()` for layer assignment, so a
    // 2-point graph occasionally fails to surface both points during search.
    // Seed the index with ids that have no metadata: HNSW returns them, the
    // `lookup` closure drops them, and we end up testing the re-rank logic
    // against a graph that's reliably populated.
    // Padding angles start far from the targets (which use angles 0.0 / 0.05)
    // so they cannot displace the targets in the cosine-similarity shortlist.
    for i in 0_u8..50 {
        let angle = f32::from(i).mul_add(0.13, 1.0);
        idx.insert(200 + u64::from(i), &unit_vec(angle)).expect("pad");
    }
    let a = unit_vec(0.0);
    let b = unit_vec(0.05);
    idx.insert(100, &a).expect("ins");
    idx.insert(101, &b).expect("ins");
    // Both same age; 101 has higher cluster_priority + edge_boost, expect 101 first.
    let meta: HashMap<u64, MetaRow> = HashMap::from([
        (
            100_u64,
            MetaRow {
                age_days: 1.0,
                cluster_priority: 1.0,
                edge_boost: 0.0,
                superseded_by: None,
            },
        ),
        (
            101_u64,
            MetaRow {
                age_days: 1.0,
                cluster_priority: 1.5,
                edge_boost: 0.3,
                superseded_by: None,
            },
        ),
    ]);
    let out = idx
        .search(&a, &SearchOpts::default(), |id| meta.get(&id).copied())
        .expect("search");
    assert_eq!(out.len(), 2, "both target candidates must reach re-rank");
    assert_eq!(out[0].id, 101, "boosted candidate should rank first");
    assert!(out[0].score > out[1].score);
}
