// SPDX-License-Identifier: Apache-2.0
//! `RunWorkflow` tool — execute a named, authored workflow as a phase-layered
//! parallel DAG of real swarm workers.
//!
//! Registration only: this module submits the `RunWorkflow` [`crate::ToolMeta`]
//! into the inventory. Execution lives in the daemon's tool dispatch, which
//! loads the named workflow from `~/.origin/workflows.toml`, computes its
//! dependency layers from each step's authored `id` / `depends_on`, and for
//! each layer dispatches one real sub-agent per step concurrently through the
//! daemon-wide swarm [`Coordinator`](origin_swarm::Coordinator) — then returns a
//! JSON summary of the run.
//!
//! This is the FAN-OUT complement to the linear `{workflow:<name>}` activation
//! path: where activation walks one skill mask at a time on the live
//! connection, `RunWorkflow` spawns workers, honours `step.args` as each
//! worker's prompt, and runs independent same-layer steps in parallel.
//!
//! [`SideEffects::Mutating`] + [`Tier::RequiresPermission`] because each spawned
//! worker may itself edit the tree (its tools come from the step skill's
//! `allowed-tools`), so the surface must be permission-gated like `Task`.

use crate::{SideEffects, Tier, Urgency};

/// JSON-Schema for the `RunWorkflow` tool input.
const RUN_WORKFLOW_INPUT_SCHEMA: &str = r#"{"type":"object","properties":{"name":{"type":"string","description":"Name of an authored workflow in ~/.origin/workflows.toml to run as a phase-layered parallel DAG of sub-agents."}},"required":["name"],"additionalProperties":false}"#;

crate::origin_tool! {
    name: "RunWorkflow",
    description: "Run a previously-authored workflow by name. Loads it from the user's workflows file, groups its steps into dependency layers, and for each layer dispatches one sub-agent per step concurrently (each step's `args` is the sub-agent's prompt; its tools come from the step skill's allowed-tools). Returns a JSON summary of the run (layers + per-step status). Complements the linear `{workflow:<name>}` skill-mask activation by actually fanning out to the swarm.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Medium,
    side_effects: SideEffects::Mutating,
    input_schema: RUN_WORKFLOW_INPUT_SCHEMA,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The inlined schema literal must be valid JSON requiring `name`.
    #[test]
    fn schema_is_valid_json_requiring_name() {
        let v: serde_json::Value = serde_json::from_str(RUN_WORKFLOW_INPUT_SCHEMA).unwrap();
        assert_eq!(v["required"][0], "name");
        assert_eq!(v["properties"]["name"]["type"], "string");
    }
}
