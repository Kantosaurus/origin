use origin_skills::{SkillFrontmatter, SkillRegistry};

#[test]
fn empty_registry_returns_none_mask() {
    let reg = SkillRegistry::new();
    assert!(reg.allowed_tools().is_none(), "no active skills -> no narrowing");
}

#[test]
fn single_active_skill_narrows_to_its_allowed_tools() {
    let mut reg = SkillRegistry::new();
    reg.activate(SkillFrontmatter {
        name: "alpha".into(),
        description: "x".into(),
        allowed_tools: vec!["Read".into(), "Bash".into()],
    });
    let mask = reg.allowed_tools().expect("mask exists");
    assert!(mask.contains("Read"));
    assert!(mask.contains("Bash"));
    assert!(!mask.contains("Edit"));
}

#[test]
fn stacked_skills_intersect_their_masks() {
    let mut reg = SkillRegistry::new();
    reg.activate(SkillFrontmatter {
        name: "alpha".into(),
        description: "x".into(),
        allowed_tools: vec!["Read".into(), "Bash".into()],
    });
    reg.activate(SkillFrontmatter {
        name: "beta".into(),
        description: "y".into(),
        allowed_tools: vec!["Read".into(), "Edit".into()],
    });
    let mask = reg.allowed_tools().expect("mask exists");
    assert!(mask.contains("Read"));
    assert!(!mask.contains("Bash"), "intersection drops Bash");
    assert!(!mask.contains("Edit"), "intersection drops Edit");
}

#[test]
fn deactivate_pops_top_of_stack() {
    let mut reg = SkillRegistry::new();
    reg.activate(SkillFrontmatter {
        name: "alpha".into(),
        description: "x".into(),
        allowed_tools: vec!["Read".into()],
    });
    reg.deactivate("alpha");
    assert!(reg.allowed_tools().is_none());
}
