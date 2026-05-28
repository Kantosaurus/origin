use origin_permission::prompt::{AlwaysAllow, AlwaysDeny};
use origin_permission::{check, Outcome};
use origin_tools::{SandboxProfile, SideEffects, Tier, ToolMeta, Urgency};

const fn meta(name: &'static str, tier: Tier) -> ToolMeta {
    ToolMeta {
        name,
        description: "test",
        tier,
        urgency: Urgency::Low,
        side_effects: SideEffects::Pure,
        input_schema: "{}",
        sandbox_profile: SandboxProfile::Inherit,
        token_budget: origin_tools::DEFAULT_TOKEN_BUDGET,
        hot: true,
    }
}

#[tokio::test]
async fn auto_allowed_bypasses_prompter() {
    let m = meta("Read", Tier::AutoAllowed);
    let decision = check(&m, "/tmp/x", &AlwaysDeny).await;
    assert_eq!(decision.outcome, Outcome::Allow);
}

#[tokio::test]
async fn requires_permission_asks_prompter_allow() {
    let m = meta("Bash", Tier::RequiresPermission);
    let decision = check(&m, "rm -rf /", &AlwaysAllow).await;
    assert_eq!(decision.outcome, Outcome::Allow);
}

#[tokio::test]
async fn requires_permission_asks_prompter_deny() {
    let m = meta("Bash", Tier::RequiresPermission);
    let decision = check(&m, "rm -rf /", &AlwaysDeny).await;
    assert_eq!(decision.outcome, Outcome::Deny);
}
