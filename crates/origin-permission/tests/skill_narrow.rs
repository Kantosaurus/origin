use origin_permission::{check_with_skills, prompt::AlwaysAllow, Outcome};
use origin_skills::SkillRegistry;
use origin_tools::{SandboxProfile, SideEffects, Tier, ToolMeta, Urgency};

const READ_META: ToolMeta = ToolMeta {
    name: "Read",
    description: "read",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: "{}",
    sandbox_profile: SandboxProfile::ReadFs,
};

const EDIT_META: ToolMeta = ToolMeta {
    name: "Edit",
    description: "edit",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Medium,
    side_effects: SideEffects::Mutating,
    input_schema: "{}",
    sandbox_profile: SandboxProfile::WriteCwd,
};

#[tokio::test]
async fn empty_registry_allows_normally() {
    let reg = SkillRegistry::new();
    let d = check_with_skills(&READ_META, "path", &AlwaysAllow, &reg).await;
    assert_eq!(d.outcome, Outcome::Allow);
}

#[tokio::test]
async fn skill_excludes_edit_returns_deny() {
    let mut reg = SkillRegistry::new();
    reg.activate(origin_skills::frontmatter::SkillFrontmatter {
        name: "no-mutate".into(),
        description: "read-only".into(),
        allowed_tools: vec!["Read".into()],
    });
    let d = check_with_skills(&EDIT_META, "path", &AlwaysAllow, &reg).await;
    assert_eq!(d.outcome, Outcome::Deny);
    assert!(d.reason.contains("skill"));
}
