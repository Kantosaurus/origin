//! Compile-time tool registry backed by the `inventory` crate.
//!
//! Each `origin_tool!` invocation submits a `ToolMeta` into the inventory.
//! `registry_iter` walks all registered tools at runtime.

use crate::{SideEffects, Tier, Urgency};

#[derive(Debug)]
pub struct ToolMeta {
    pub name: &'static str,
    pub description: &'static str,
    pub tier: Tier,
    pub urgency: Urgency,
    pub side_effects: SideEffects,
    pub input_schema: &'static str,
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
