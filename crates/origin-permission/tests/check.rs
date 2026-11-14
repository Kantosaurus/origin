// SPDX-License-Identifier: Apache-2.0
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

/// C2: a headless prompter denies and reports that nobody can be asked; a
/// human-facing prompter keeps the default `can_ask() == true`.
#[tokio::test]
async fn no_prompter_denies_and_cannot_ask() {
    use origin_permission::prompt::{NoPrompter, Prompter};
    let m = meta("Bash", Tier::RequiresPermission);
    assert_eq!(check(&m, "ls", &NoPrompter).await.outcome, Outcome::Deny);
    assert!(!NoPrompter.can_ask());
    assert!(AlwaysAllow.can_ask() && AlwaysDeny.can_ask());
}
