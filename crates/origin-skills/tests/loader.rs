use origin_skills::{load_skills_dir, Skill};
use std::fs;
use tempfile::tempdir;

fn write_skill(root: &std::path::Path, name: &str, body: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).expect("mkdir");
    let contents =
        format!("---\nname: {name}\ndescription: A test skill.\nallowed-tools: [Read]\n---\n{body}\n");
    fs::write(dir.join("SKILL.md"), contents).expect("write");
}

#[test]
fn loads_skills_from_directory() {
    let tmp = tempdir().expect("tmp");
    write_skill(tmp.path(), "alpha", "alpha body");
    write_skill(tmp.path(), "beta", "beta body");

    let skills: Vec<Skill> = load_skills_dir(tmp.path()).expect("load");
    let names: Vec<&str> = skills.iter().map(|s| s.front.name.as_str()).collect();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta"));
}

#[test]
fn dedupes_by_body_hash() {
    let tmp = tempdir().expect("tmp");
    write_skill(tmp.path(), "alpha", "shared body text");
    write_skill(tmp.path(), "alpha-copy", "shared body text");

    let skills = load_skills_dir(tmp.path()).expect("load");
    // Two distinct names but the body hash should collide -> 1 dedupe-key class.
    let mut hashes: Vec<_> = skills.iter().map(|s| s.body_hash.0).collect();
    #[allow(clippy::stable_sort_primitive)] // test — order not significant, dedup correctness only
    hashes.sort();
    hashes.dedup();
    assert_eq!(hashes.len(), 1, "expected one unique body hash");
}

#[test]
fn ignores_subdirs_without_skill_md() {
    let tmp = tempdir().expect("tmp");
    write_skill(tmp.path(), "alpha", "body");
    std::fs::create_dir_all(tmp.path().join("not-a-skill")).expect("mkdir");
    let skills = load_skills_dir(tmp.path()).expect("load");
    assert_eq!(skills.len(), 1);
}
