//! `origin_tool!` macro — registers a tool's metadata into the inventory.

#[macro_export]
macro_rules! origin_tool {
    // Full form with sandbox AND token_budget AND hot.
    (
        name: $name:literal,
        description: $desc:literal,
        tier: $tier:expr,
        urgency: $urg:expr,
        side_effects: $sfx:expr,
        input_schema: $schema:expr,
        sandbox: $sandbox:expr,
        token_budget: $budget:expr,
        hot: $hot:expr
        $(,)?
    ) => {
        inventory::submit! {
            $crate::ToolMeta {
                name: $name,
                description: $desc,
                tier: $tier,
                urgency: $urg,
                side_effects: $sfx,
                input_schema: $schema,
                sandbox_profile: $sandbox,
                token_budget: $budget,
                hot: $hot,
            }
        }
    };
    // Full form with sandbox AND token_budget, default hot: true.
    (
        name: $name:literal,
        description: $desc:literal,
        tier: $tier:expr,
        urgency: $urg:expr,
        side_effects: $sfx:expr,
        input_schema: $schema:expr,
        sandbox: $sandbox:expr,
        token_budget: $budget:expr
        $(,)?
    ) => {
        inventory::submit! {
            $crate::ToolMeta {
                name: $name,
                description: $desc,
                tier: $tier,
                urgency: $urg,
                side_effects: $sfx,
                input_schema: $schema,
                sandbox_profile: $sandbox,
                token_budget: $budget,
                hot: true,
            }
        }
    };
    // Sandbox set, default token_budget, default hot: true.
    (
        name: $name:literal,
        description: $desc:literal,
        tier: $tier:expr,
        urgency: $urg:expr,
        side_effects: $sfx:expr,
        input_schema: $schema:expr,
        sandbox: $sandbox:expr
        $(,)?
    ) => {
        inventory::submit! {
            $crate::ToolMeta {
                name: $name,
                description: $desc,
                tier: $tier,
                urgency: $urg,
                side_effects: $sfx,
                input_schema: $schema,
                sandbox_profile: $sandbox,
                token_budget: $crate::DEFAULT_TOKEN_BUDGET,
                hot: true,
            }
        }
    };
    // Default sandbox AND default token_budget, default hot: true.
    (
        name: $name:literal,
        description: $desc:literal,
        tier: $tier:expr,
        urgency: $urg:expr,
        side_effects: $sfx:expr,
        input_schema: $schema:expr
        $(,)?
    ) => {
        inventory::submit! {
            $crate::ToolMeta {
                name: $name,
                description: $desc,
                tier: $tier,
                urgency: $urg,
                side_effects: $sfx,
                input_schema: $schema,
                sandbox_profile: ::origin_sandbox::SandboxProfile::Inherit,
                token_budget: $crate::DEFAULT_TOKEN_BUDGET,
                hot: true,
            }
        }
    };
}
