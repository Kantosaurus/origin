// SPDX-License-Identifier: Apache-2.0
//! `AuthorWorkflow` tool — synthesise a runnable workflow from a goal.
//!
//! Registration only: this module submits the `AuthorWorkflow`
//! [`crate::ToolMeta`] into the inventory. Execution lives in the daemon's tool
//! dispatch, which builds a [`origin_workflowgen::SkillCatalog`] from the live
//! skill registry, calls [`origin_workflowgen::author_and_render`], persists the
//! result to `~/.origin/workflows.toml`, and returns the rendered TOML so the
//! model sees what it created. The authored workflow is then immediately
//! runnable via the existing `{workflow:<name>}` activation path.
//!
//! [`SideEffects::Mutating`] because the tool persists a file. The tier is
//! [`Tier::RequiresPermission`], matching the workflow-activation surface: a
//! tool that writes to the user's config must be permission-gated.
//!
//! The `input_schema:` literal below is byte-identical (modulo serde's stable
//! key ordering) to [`origin_workflowgen::tool_input_schema()`]; the
//! `schema_matches_crate` test enforces that the two never drift apart.

use crate::{SideEffects, Tier, Urgency};

/// JSON-Schema for the `AuthorWorkflow` tool, inlined as a `&'static str`
/// literal for the `origin_tool!` macro's `input_schema:` arm. Kept in
/// lock-step with [`origin_workflowgen::tool_input_schema()`] by the drift-guard
/// test below.
const AUTHOR_WORKFLOW_INPUT_SCHEMA: &str = r#"{"type":"object","properties":{"goal":{"type":"string","description":"Natural-language description of what the workflow should accomplish. The planner decomposes it into an ordered explore/plan/implement/verify pipeline over the available skills."},"name":{"type":"string","description":"Optional explicit workflow name. When omitted a slug is derived from the goal."}},"required":["goal"],"additionalProperties":false}"#;

crate::origin_tool! {
    name: "AuthorWorkflow",
    description: "Author a new, runnable workflow from a natural-language `goal`. Decomposes the goal into an ordered explore/plan/implement/verify pipeline over the skills currently available, persists it to the user's workflows file, and returns the rendered TOML plus the chosen name. The result is immediately runnable via `{workflow:<name>}`. Pass an optional `name` to override the auto-derived slug.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Low,
    side_effects: SideEffects::Mutating,
    input_schema: AUTHOR_WORKFLOW_INPUT_SCHEMA,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Drift guard: the inlined `&'static str` schema literal must parse to the
    /// exact same JSON value the `origin-workflowgen` crate produces.
    #[test]
    fn schema_matches_crate() {
        let from_literal: serde_json::Value =
            serde_json::from_str(AUTHOR_WORKFLOW_INPUT_SCHEMA).unwrap();
        assert_eq!(from_literal, origin_workflowgen::tool_input_schema());
    }
}
