// SPDX-License-Identifier: Apache-2.0
//! Compile-time tool registry backed by the `inventory` crate.
//!
//! Each `origin_tool!` invocation submits a `ToolMeta` into the inventory.
//! `registry_iter` walks all registered tools at runtime.

use crate::{SideEffects, Tier, Urgency};
use origin_sandbox::SandboxProfile;

#[derive(Debug)]
pub struct ToolMeta {
    pub name: &'static str,
    pub description: &'static str,
    pub tier: Tier,
    pub urgency: Urgency,
    pub side_effects: SideEffects,
    pub input_schema: &'static str,
    /// Per-tool sandbox profile applied to child processes this tool spawns.
    /// Defaults to [`SandboxProfile::Inherit`] (no extra confinement); tools
    /// that exec untrusted binaries override this to `Shell`, `WriteCwd`,
    /// etc. via the optional `sandbox: …` arm of [`crate::origin_tool!`].
    pub sandbox_profile: SandboxProfile,
    /// Approximate token budget for this tool's serialised result. The
    /// envelope's `ResultWriter` truncates / elides at this cap. Default 25k.
    pub token_budget: u32,
    /// "Hot" tools have their full schema embedded in the system prompt.
    /// "Deferred" tools advertise only {name, description}; their schemas
    /// are fetched on demand via `ToolSearch`.
    pub hot: bool,
}

inventory::collect!(ToolMeta);

// `must_use` is already implied by `Iterator`; allow the redundancy so the
// public API is self-documenting and the name stays consistent with the
// module (registry_iter lives in registry).
#[allow(clippy::double_must_use)]
#[allow(clippy::module_name_repetitions)]
#[must_use]
pub fn registry_iter() -> impl Iterator<Item = &'static ToolMeta> {
    inventory::iter::<ToolMeta>.into_iter()
}
