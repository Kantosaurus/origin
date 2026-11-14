// SPDX-License-Identifier: Apache-2.0
//! `origin_tool!` macro — registers a tool's metadata into the inventory.

#[macro_export]
macro_rules! origin_tool {
    // Full form with sandbox AND hot.
    (
        name: $name:literal,
        description: $desc:literal,
        tier: $tier:expr,
        urgency: $urg:expr,
        side_effects: $sfx:expr,
        input_schema: $schema:expr,
        sandbox: $sandbox:expr,
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
                hot: $hot,
            }
        }
    };
    // Sandbox set, default hot: true.
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
                hot: true,
            }
        }
    };
    // Default sandbox, default hot: true.
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
                hot: true,
            }
        }
    };
}
