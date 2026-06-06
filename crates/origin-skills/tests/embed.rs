// SPDX-License-Identifier: Apache-2.0
use origin_skills::{Skill, SkillEmbedder, SkillHash};

fn make_skill(name: &str, body: &str) -> Skill {
    Skill {
        front: origin_skills::frontmatter::SkillFrontmatter {
            name: name.into(),
            description: "test".into(),
            allowed_tools: vec![],
        },
        body: body.into(),
        body_hash: SkillHash(*blake3::hash(body.as_bytes()).as_bytes()),
        source: std::path::PathBuf::from(format!("/skills/{name}/SKILL.md")),
    }
}

#[test]
fn upsert_and_recall_skill() {
    let mut index = origin_mem::MemIndex::new();
    let mut embedder = SkillEmbedder::stub_for_tests();
    let alpha = make_skill("alpha", "learn how to write tests");

    let id = embedder.upsert(&mut index, &alpha).expect("upsert");
    assert!(id > 0, "ulid lower-64 should be non-zero");

    // hnsw_rs 0.3 assigns graph layers with `StdRng::from_os_rng()`, so a tiny
    // index (here a single point) can non-deterministically fail to return its
    // only element from `search`. Pad the graph with metadata-less filler ids —
    // the `lookup` closure returns `None` for them so they are dropped before
    // re-ranking, leaving `alpha` the sole survivor. Mirrors the padding pattern
    // in origin-mem's own index tests.
    for i in 0_u64..64 {
        let pad = embedder.embed_for_tests(&format!("skill-recall-padding-{i}"));
        index.insert(1_000_000_u64 + i, &pad).expect("pad insert");
    }

    let query_vec = embedder.embed_for_tests("how do i write tests");
    let opts = origin_mem::SearchOpts::default();
    let hits = index
        .search(&query_vec, &opts, |qid| {
            // Only `alpha` carries metadata; padding ids return `None` and are
            // filtered out, so recall must surface `alpha`.
            (qid == id).then_some(origin_mem::MetaRow {
                age_days: 0.0,
                cluster_priority: 1.0,
                edge_boost: 0.0,
                superseded_by: None,
            })
        })
        .expect("search");
    assert!(!hits.is_empty(), "recall should return skill candidate");
    assert_eq!(hits[0].id, id);
}
