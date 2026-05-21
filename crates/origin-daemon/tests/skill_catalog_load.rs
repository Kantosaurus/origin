//! Integration test: `SkillCatalog` loads from a real directory layout.

use origin_daemon::skill_catalog::SkillCatalog;
use std::path::Path;

fn write_skill(dir: &Path, name: &str) {
    let skill_dir = dir.join(name);
    std::fs::create_dir_all(&skill_dir).expect("mkdir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!(
            "---\nname: {name}\ndescription: integration\nallowed-tools: [\"Read\"]\n---\nbody\n"
        ),
    )
    .expect("write");
}

#[test]
fn catalog_finds_skills_in_subdirs() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_skill(dir.path(), "alpha");
    write_skill(dir.path(), "beta");
    let cat = SkillCatalog::load_from(dir.path()).expect("load");
    assert_eq!(cat.len(), 2);
    assert!(cat.find("alpha").is_some());
    assert!(cat.find("beta").is_some());
}
