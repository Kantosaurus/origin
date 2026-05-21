use origin_skills::{load_all, load_embedded};
use tempfile::tempdir;

#[test]
fn embedded_includes_all_14_superpowers_skills() {
    let skills = load_embedded();
    let names: Vec<&str> = skills.iter().map(|s| s.front.name.as_str()).collect();
    let expected = [
        "brainstorming", "dispatching-parallel-agents", "executing-plans",
        "finishing-a-development-branch", "receiving-code-review",
        "requesting-code-review", "subagent-driven-development",
        "systematic-debugging", "test-driven-development",
        "using-git-worktrees", "using-superpowers",
        "verification-before-completion", "writing-plans", "writing-skills",
    ];
    for want in expected {
        assert!(names.iter().any(|n| *n == want), "missing embedded skill: {want}; got {names:?}");
    }
    assert_eq!(skills.len(), 14, "expected exactly 14 embedded skills, got {}", skills.len());
}

#[test]
fn user_skill_overrides_embedded_by_name() {
    let dir = tempdir().unwrap();
    let user_root = dir.path();
    let brainstorm_dir = user_root.join("brainstorming");
    std::fs::create_dir_all(&brainstorm_dir).unwrap();
    std::fs::write(
        brainstorm_dir.join("SKILL.md"),
        "---\nname: brainstorming\ndescription: user override\n---\n# user body\n",
    ).unwrap();

    let all = load_all(user_root).unwrap();
    let bs = all.iter().find(|s| s.front.name == "brainstorming").unwrap();
    assert_eq!(bs.front.description, "user override");
    assert_eq!(all.len(), 14, "merging should not change count when override matches one embedded");
}
