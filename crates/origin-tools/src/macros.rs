//! `origin_tool!` macro — registers a tool's metadata into the inventory.
//!
//! The optional `sandbox: <SandboxProfile>` arm sets the per-tool sandbox
//! profile (P11.5). When omitted, the meta defaults to
//! `SandboxProfile::Inherit` (no extra confinement) so existing call-sites
//! compile unchanged.

#[macro_export]
macro_rules! origin_tool {
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
            }
        }
    };
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
            }
        }
    };
}
