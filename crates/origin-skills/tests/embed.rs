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

    let query_vec = embedder.embed_for_tests("how do i write tests");
    let opts = origin_mem::SearchOpts::default();
    let hits = index
        .search(&query_vec, &opts, |_id| {
            Some(origin_mem::MetaRow {
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
