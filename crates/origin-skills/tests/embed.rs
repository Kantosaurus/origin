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

/// Production path: `with_embedder(Arc<Embedder>)` embeds the skill body through
/// a real ONNX `origin_mem::Embedder` and inserts a normalised 384-dim vector
/// into the index. Uses origin-mem's stub `MiniLM` fixture so the test exercises
/// the actual `Inner::Real` arm (not the deterministic hash stub).
#[test]
fn with_embedder_inserts_real_embedding() {
    use std::sync::Arc;
    // The stub ONNX MiniLM model shipped for origin-mem's own embedder tests.
    let model = "../origin-mem/tests/fixtures/stub_minilm.onnx";
    // If the fixture can't be located (e.g. an unusual CWD), skip rather than
    // fail — the wiring still compiles and the stub-path test above covers the
    // index round-trip. Build two handles: one drives `upsert`, one the query.
    let Ok(query_embedder) = origin_mem::Embedder::from_path(model) else {
        return;
    };
    let Ok(upsert_embedder) = origin_mem::Embedder::from_path(model) else {
        return;
    };

    let mut index = origin_mem::MemIndex::new();
    let mut skill_embedder = SkillEmbedder::with_embedder(Arc::new(upsert_embedder));
    let skill = make_skill("real", "embed this skill body through onnx");

    let id = skill_embedder
        .upsert(&mut index, &skill)
        .expect("real upsert must succeed");
    assert!(id > 0, "body-hash lower-64 should be non-zero");

    // Re-querying with the same vector the embedder produces must surface this
    // skill id (the index now actually contains a real, non-stub embedding).
    let real = query_embedder.embed(&skill.body).expect("embed query");
    let mut q = [0_f32; origin_mem::EMBED_DIM];
    let n = real.len().min(origin_mem::EMBED_DIM);
    q[..n].copy_from_slice(&real[..n]);

    // Pad so hnsw_rs reliably returns the single real point (see note above).
    for i in 0_u64..64 {
        let mut pad = [0_f32; origin_mem::EMBED_DIM];
        let slot = usize::try_from(i).unwrap_or(0) % origin_mem::EMBED_DIM;
        pad[slot] = 1.0; // unit vector along one axis — already L2-normalised.
        index.insert(2_000_000_u64 + i, &pad).expect("pad insert");
    }

    let opts = origin_mem::SearchOpts::default();
    let hits = index
        .search(&q, &opts, |qid| {
            (qid == id).then_some(origin_mem::MetaRow {
                age_days: 0.0,
                cluster_priority: 1.0,
                edge_boost: 0.0,
                superseded_by: None,
            })
        })
        .expect("search");
    assert!(
        hits.iter().any(|h| h.id == id),
        "real-embedded skill must be recallable from the index"
    );
}
