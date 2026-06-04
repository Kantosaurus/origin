// SPDX-License-Identifier: Apache-2.0
//! Phase-layered parallel workflow execution.
//!
//! The linear sequencer in [`crate::workflow_progress`] walks a workflow one
//! step at a time, activating each step's *skill mask* on the live connection.
//! It deliberately ignores `step.args` and never fans out. This module is the
//! complementary **fan-out** path used by the `RunWorkflow` tool / `origin
//! workflow run` verb: it reads the authored phase-layered DAG carried on each
//! [`WorkflowStep`](crate::workflows::WorkflowStep)'s `id` / `depends_on`,
//! groups steps into dependency layers, and for each layer dispatches **one
//! real swarm worker per step concurrently** through the daemon-wide
//! [`Coordinator`](origin_swarm::Coordinator) — exactly the spawn-all-then-await
//! shape the `Task` tool uses (`task::task_spawn` + `task::task_await`). Each
//! worker's prompt is the step's `args`, falling back to the step skill's
//! catalog description when `args` is empty; its `allowed_tools` come from the
//! step skill's declared `allowed-tools`, defaulting to a read+edit set when the
//! skill is absent or declares none.
//!
//! The linear `{workflow:<name>}` activation path is untouched: it does not call
//! into this module, so its behaviour stays byte-identical.

use std::collections::BTreeMap;

use origin_plan::ActorId;
use origin_swarm::{Budget, Coordinator, ReportStatus, SwarmError, WorkerSpec};
use serde::Serialize;
use thiserror::Error;

use crate::skill_catalog::SkillCatalog;
use crate::workflows::{Workflow, WorkflowStep};

/// Default tool allow-list for a workflow worker whose step skill is absent
/// from the catalog or declares no `allowed-tools`. A read + edit set: enough
/// to investigate and make a focused change, but not to run arbitrary commands.
const DEFAULT_STEP_TOOLS: &[&str] = &["Read", "Grep", "Glob", "Edit", "Write"];

/// Per-step worker budget. Generous wall/token ceilings (a step is a full
/// sub-agent turn) with the same tool-call cap the `Task` default uses.
const STEP_BUDGET: Budget = Budget::new(
    /* max_wall_ms */ 300_000,
    /* max_input_tokens */ 1_000_000,
    /* max_output_tokens */ 256_000,
    /* max_tool_calls */ 32,
);

/// Errors raised while computing layers or running a workflow.
#[derive(Debug, Error)]
pub enum RunError {
    /// The workflow's dependency graph could not be layered (cycle or dangling
    /// edge). Carries the underlying planner error rendering.
    #[error("workflow dependency graph: {0}")]
    Layering(String),
    /// A worker spawn or await propagated a swarm-layer error.
    #[error("swarm: {0}")]
    Swarm(#[from] SwarmError),
}

/// Per-step outcome inside a [`RunReport`].
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StepReport {
    /// Zero-based position of the step in the workflow's `steps` vector.
    pub index: usize,
    /// The step's skill name.
    pub skill: String,
    /// The dependency layer the step ran in (0 == first).
    pub layer: usize,
    /// Terminal status, lower-snake-cased (`completed`, `goal_unreachable`,
    /// `budget_exhausted`, `aborted`).
    pub status: String,
}

/// Aggregate result of running a workflow as a phase-layered parallel DAG.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunReport {
    /// The workflow name.
    pub name: String,
    /// Number of dependency layers executed.
    pub layers: usize,
    /// Per-step outcomes, in execution order (layer-major, then step order).
    pub steps: Vec<StepReport>,
}

/// Map a daemon [`Workflow`]'s steps into dependency layers of **positions**
/// (indices into `workflow.steps`), via [`origin_workflowgen::execution_layers`].
///
/// Each daemon step's `id` / `depends_on` carry the authored phase-layered DAG;
/// we re-derive the layering rather than trusting a stored layer field so a
/// hand-edited `workflows.toml` is validated (a cycle or dangling edge surfaces
/// as [`RunError::Layering`]). The returned layers reference *positions* so the
/// caller can index `workflow.steps` directly regardless of the `id` values.
///
/// # Errors
/// Returns [`RunError::Layering`] if the graph has a cycle or an edge points at
/// an id no step carries.
pub fn compute_layers(workflow: &Workflow) -> Result<Vec<Vec<usize>>, RunError> {
    // Build a workflowgen spec mirroring the daemon steps' id/depends_on so we
    // can reuse the crate's Kahn layering (single source of truth, novel-impl
    // rule: no second topological sort).
    let spec = origin_workflowgen::WorkflowSpec {
        name: workflow.name.clone(),
        description: workflow.description.clone().unwrap_or_default(),
        steps: workflow
            .steps
            .iter()
            .map(|s| origin_workflowgen::WorkflowStep {
                id: origin_workflowgen::StepId(s.id),
                skill: s.skill.clone(),
                args: s.args.clone().unwrap_or_default(),
                depends_on: s
                    .depends_on
                    .iter()
                    .map(|d| origin_workflowgen::StepId(*d))
                    .collect(),
            })
            .collect(),
    };
    let id_layers =
        origin_workflowgen::execution_layers(&spec).map_err(|e| RunError::Layering(e.to_string()))?;

    // `execution_layers` returns ids; map each id back to its position in
    // `workflow.steps` so the caller can index directly. An id with no matching
    // step cannot occur (every layer id came from a step), but we skip rather
    // than panic if it somehow does.
    let pos_by_id: BTreeMap<usize, usize> = workflow
        .steps
        .iter()
        .enumerate()
        .map(|(pos, s)| (s.id, pos))
        .collect();
    let layers = id_layers
        .into_iter()
        .map(|layer| {
            layer
                .into_iter()
                .filter_map(|id| pos_by_id.get(&id.index()).copied())
                .collect()
        })
        .collect();
    Ok(layers)
}

/// Build the [`WorkerSpec`] for `step`, deriving the prompt and tool allow-list.
///
/// - Prompt: `step.args` when non-empty, else the step skill's catalog
///   description (so a bare `[[workflows.steps]] skill = "x"` still gives the
///   worker a goal), else a generic "run the <skill> skill" fallback.
/// - Allowed tools: the step skill's declared `allowed-tools`, else
///   [`DEFAULT_STEP_TOOLS`]. `Task` is always stripped by the worker substrate,
///   so a workflow step can never recurse into another workflow's swarm.
#[must_use]
pub fn step_worker_spec(step: &WorkflowStep, catalog: &SkillCatalog) -> WorkerSpec {
    let skill = catalog.find(&step.skill);
    let goal = step
        .args
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            skill
                .map(|s| s.front.description.clone())
                .filter(|d| !d.trim().is_empty())
        })
        .unwrap_or_else(|| format!("run the {} skill", step.skill));
    let allowed_tools = match skill {
        Some(s) if !s.front.allowed_tools.is_empty() => s.front.allowed_tools.clone(),
        _ => DEFAULT_STEP_TOOLS.iter().map(|t| (*t).to_string()).collect(),
    };
    WorkerSpec {
        goal,
        allowed_tools,
        budget: STEP_BUDGET,
        workspace: None,
        parent_actor: ActorId::new(0),
        model: None,
    }
}

/// Run `workflow` as a phase-layered parallel DAG against `coordinator`.
///
/// For each dependency layer (computed by [`compute_layers`]): build a
/// [`WorkerSpec`] per step, `spawn` **every** worker in the layer up front
/// (so they run concurrently on the coordinator's swarm pool), then
/// `await_completion` on each — the whole layer is joined before the next layer
/// starts, so a downstream step never begins before its dependencies finish.
/// Aggregates every worker's terminal status into a [`RunReport`].
///
/// # Errors
/// - [`RunError::Layering`] if the workflow's dependency graph is invalid.
/// - [`RunError::Swarm`] if a worker spawn or await fails at the swarm layer.
pub async fn run_workflow(
    workflow: &Workflow,
    coordinator: &Coordinator,
    catalog: &SkillCatalog,
) -> Result<RunReport, RunError> {
    let layers = compute_layers(workflow)?;
    let mut steps_out: Vec<StepReport> = Vec::with_capacity(workflow.steps.len());

    for (layer_idx, layer) in layers.iter().enumerate() {
        // Spawn EVERY step in this layer before awaiting any of them — this is
        // what makes same-layer steps run in parallel (mirrors task.rs's
        // spawn-all-then-await turn shape).
        let mut handles = Vec::with_capacity(layer.len());
        for &pos in layer {
            let step = &workflow.steps[pos];
            let spec = step_worker_spec(step, catalog);
            let handle = coordinator.spawn(spec).await?;
            handles.push((pos, handle));
        }
        // Await the whole layer before starting the next.
        for (pos, handle) in handles {
            let report = coordinator.await_completion(&handle).await?;
            let step = &workflow.steps[pos];
            steps_out.push(StepReport {
                index: pos,
                skill: step.skill.clone(),
                layer: layer_idx,
                status: status_label(report.status).to_string(),
            });
        }
    }

    Ok(RunReport {
        name: workflow.name.clone(),
        layers: layers.len(),
        steps: steps_out,
    })
}

/// Lower-snake-case label for a worker's terminal status (matches the `Task`
/// tool's vocabulary so downstream renderers stay consistent).
const fn status_label(status: ReportStatus) -> &'static str {
    match status {
        ReportStatus::Completed => "completed",
        ReportStatus::GoalUnreachable => "goal_unreachable",
        ReportStatus::BudgetExhausted => "budget_exhausted",
        ReportStatus::Aborted => "aborted",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::workflows::Workflow;

    fn step(id: usize, skill: &str, deps: &[usize]) -> WorkflowStep {
        WorkflowStep {
            id,
            skill: skill.into(),
            args: None,
            depends_on: deps.to_vec(),
        }
    }

    /// Diamond DAG: 0 -> {1,2} -> 3. `compute_layers` must place 0 alone first,
    /// 1 and 2 together in the middle, and 3 alone last.
    #[test]
    fn compute_layers_groups_independent_steps_into_one_layer() {
        let wf = Workflow {
            name: "diamond".into(),
            description: None,
            steps: vec![
                step(0, "a", &[]),
                step(1, "b", &[0]),
                step(2, "c", &[0]),
                step(3, "d", &[1, 2]),
            ],
        };
        let layers = compute_layers(&wf).unwrap();
        assert_eq!(layers, vec![vec![0], vec![1, 2], vec![3]]);
    }

    /// A cyclic graph has no layering -> `RunError::Layering`.
    #[test]
    fn compute_layers_rejects_cycle() {
        let wf = Workflow {
            name: "cycle".into(),
            description: None,
            steps: vec![step(0, "a", &[1]), step(1, "b", &[0])],
        };
        assert!(matches!(compute_layers(&wf), Err(RunError::Layering(_))));
    }
}
