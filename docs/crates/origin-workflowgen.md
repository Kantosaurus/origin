# origin-workflowgen

> Deterministic offline workflow authoring: decompose a natural-language goal into an ordered, validated skill pipeline.

## Purpose

`origin-workflowgen` is the *authoring* layer that synthesises a brand-new
workflow on demand from a plain-English goal plus the set of skills the agent
actually has. The daemon already ships a *run* substrate (a fixed
`workflows.toml` loader); this crate produces those documents. Authoring is a
**pure, deterministic function of `(goal, catalog)`** — no LLM round-trip — so
identical inputs always yield byte-identical output, and the result is free of
network/process concerns and trivially unit-testable.

## Public API surface

| Item | Kind | Description |
|------|------|-------------|
| `author_workflow` | fn | Decompose a goal into a validated `WorkflowSpec`. |
| `author_and_render` | fn | `author_workflow` + render to `workflows.toml` text in one call. |
| `WorkflowSpec` | struct | Named, described, ordered list of steps (daemon-compatible). |
| `WorkflowStep` | struct | `{ id, skill, args, depends_on }` — one pipeline slot. |
| `StepId` | struct | Transparent newtype over a step's zero-based position. |
| `SkillCatalog` / `SkillInfo` | struct | The available skills the planner matches against. |
| `Phase` | enum | Lifecycle phases: `Explore → Plan → Implement → Verify`. |
| `detected_phases` | fn | Which phases a goal's tokens explicitly request. |
| `score_skill` | fn | Keyword-overlap score (name-weighted) of a skill vs. query tokens. |
| `execution_layers` | fn | Topological (Kahn) layering of the dependency DAG. |
| `tokenize` / `slugify` | fn | Text helpers. |
| `tool_input_schema` | fn | JSON schema for the `AuthorWorkflow` builtin tool. |
| `WorkflowGenError` | enum | `EmptyGoal` / `EmptyCatalog` / `NoMatch` / `UnknownSkill` / `UnknownDependency` / `CyclicDependency` / `Serialize`. |

## Key types

```rust
pub struct WorkflowStep {
    pub id: StepId,
    pub skill: String,
    pub args: String,                 // skipped from TOML when empty
    pub depends_on: Vec<StepId>,      // phase-layered DAG edges
}

pub struct WorkflowSpec { pub name: String, pub description: String, pub steps: Vec<WorkflowStep> }

pub fn author_workflow(goal: &str, catalog: &SkillCatalog) -> Result<WorkflowSpec>;
pub fn execution_layers(spec: &WorkflowSpec) -> Result<Vec<Vec<StepId>>>;
impl WorkflowSpec {
    pub fn validate(&self, catalog: &SkillCatalog) -> Result<()>;
    pub fn to_toml(&self) -> Result<String>;   // daemon-shaped workflows.toml
}
```

## How it works

A three-stage deterministic heuristic planner:

```
goal ─► tokenize ─► drop stop-words
                       │
   1. Phase classification: each Phase has trigger lexemes
      ("investigate" → Explore, "test" → Verify). Goal tokens vote for phases;
      none detected ⇒ full default Explore→Plan→Implement→Verify pipeline.
                       │
   2. Skill matching: for each active phase, score every catalog skill by
      keyword overlap of (phase lexemes + goal tokens) vs (skill name + desc).
      score_skill weights name=4, description=2, substring=1; ties break to the
      lowest catalog index. Highest scorer wins the slot (each skill at most once).
                       │
   3. Validation: every emitted step.skill is guaranteed present in the catalog;
      depends_on links each step to ALL steps of the previous non-empty phase,
      so same-phase steps are parallelizable. A non-empty goal vs non-empty
      catalog never yields an empty workflow (falls back to best whole-goal match).
```

`execution_layers` runs Kahn's algorithm grouped by layer: layer 0 is every
step with no unmet deps, each next layer's deps are all satisfied earlier —
yielding the concurrency-safe groups the daemon's `RunWorkflow` fans out.
`to_toml` emits the exact `{ schema_version, [[workflows]] }` document the
daemon's `load_from` parses verbatim (a round-trip test proves it).

## Dependencies & features

- Runtime deps: `serde` (derive), `serde_json` (tool schema), `toml`
  (serialization), `thiserror`. No `origin-skills`/daemon dependency — the tiny
  `SkillInfo`/`SkillCatalog` are owned here so Phase 2 adapts whatever registry
  it has. `#![forbid(unsafe_code)]`. No Cargo features.

## Used by

`Grep "origin-workflowgen" glob "crates/*/Cargo.toml"` →

- `crates/origin-cli/Cargo.toml`
- `crates/origin-daemon/Cargo.toml`
- `crates/origin-tools/Cargo.toml`
- `crates/origin-workflowgen/Cargo.toml` (self)

`origin-tools` exposes the `AuthorWorkflow` builtin tool; the daemon persists
the generated spec via its workflows save path; the CLI offers
`origin workflow author <goal>`.

## Testing

Comprehensive in-file `#[cfg(test)] mod tests`: tokenization, name-vs-description
scoring weights, stop-word/dedup handling, canonical phase ordering, multi-intent
pipelines, bare-goal default pipeline, every error variant (`EmptyGoal`,
`EmptyCatalog`, `NoMatch`, `UnknownSkill`), determinism (byte-identical output
and TOML), and a round-trip proving the emitted TOML parses into the daemon's
`{name, steps:[{skill, args}]}` shape.

## See also

- [Skills subsystem](../subsystems/skills.md)
- [Swarm & orchestration subsystem](../subsystems/swarm-and-orchestration.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
