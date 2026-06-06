// SPDX-License-Identifier: Apache-2.0
//! Dynamic workflow authoring: turn a natural-language goal into an ordered,
//! validated skill pipeline — entirely offline, with no model call.
//!
//! `origin` already ships a *run* substrate: a fixed `workflows.toml` loader
//! (see `origin-daemon`'s `workflows.rs`) that executes a named chain of skills
//! one step per turn. What it lacks is an *authoring* layer — a way to
//! synthesise a brand-new workflow on demand from a plain-English goal plus the
//! set of skills the agent actually has. This crate is that layer.
//!
//! ## Novelty: a deterministic heuristic planner (no LLM round-trip)
//!
//! Authoring a workflow with an LLM would cost a full generation turn and is
//! non-deterministic. Instead this crate decomposes the goal *locally*:
//!
//! 1. **Phase classification.** A goal is broken into the canonical software
//!    lifecycle [`Phase`]s — Explore → Plan → Implement → Verify. Each phase
//!    has a small set of *trigger lexemes* (e.g. "investigate"/"understand" for
//!    Explore, "test"/"verify" for Verify). The goal's tokens vote for which
//!    phases are explicitly requested; absent any vote, a sensible default
//!    pipeline is assembled so a bare goal like "add a feature" still yields a
//!    full Explore→Plan→Implement→Verify chain.
//! 2. **Skill matching.** For every active phase we score each catalog skill by
//!    keyword overlap between (a) the phase's lexemes plus the goal tokens and
//!    (b) the skill's name + description tokens (see [`score_skill`]). The
//!    highest-scoring catalog skill wins the slot; phases with no positive
//!    match are dropped (we never invent a skill that isn't in the catalog).
//! 3. **Validation.** Every emitted step's `skill` is guaranteed to exist in
//!    the catalog ([`WorkflowSpec::validate`]); a non-empty goal against a
//!    non-empty catalog never yields an empty workflow (we fall back to the
//!    single best whole-goal match).
//!
//! The whole pipeline is a pure function of `(goal, catalog)`: identical inputs
//! always produce byte-identical output (deterministic ordering, stable
//! tie-breaks by catalog index). That makes it trivially unit-testable and free
//! of network/process concerns.
//!
//! ## Shape compatibility with the daemon
//!
//! [`WorkflowSpec`] and [`WorkflowStep`] are serde-compatible with the daemon's
//! `WorkflowsFile`/`Workflow`/`WorkflowStep`. [`WorkflowSpec::to_toml`] emits a
//! single-workflow `workflows.toml` document that the daemon's `load_from`
//! parses verbatim. Phase 2 maps `WorkflowSpec` → `origin_*::workflows::Workflow`
//! 1:1 (see the crate docs' "Phase 2 wiring" notes and the structured report).
//!
//! ```
//! use origin_workflowgen::{author_workflow, SkillCatalog, SkillInfo};
//!
//! let catalog = SkillCatalog::new(vec![
//!     SkillInfo::new("scout", "explore and understand the codebase"),
//!     SkillInfo::new("plan", "plan the implementation approach"),
//!     SkillInfo::new("impeccable", "implement and build the feature"),
//!     SkillInfo::new("verify", "run tests and verify the change works"),
//! ]);
//! let spec = author_workflow("explore the auth code then implement and test login", &catalog)
//!     .expect("author");
//! assert!(!spec.steps.is_empty());
//! // every step references a real catalog skill
//! assert!(spec.validate(&catalog).is_ok());
//! ```

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum number of characters of the goal copied verbatim into a step's
/// `args`. Keeps generated TOML compact and avoids pathological multi-kilobyte
/// goal strings ballooning every step.
const MAX_ARG_LEN: usize = 240;

/// One step of an authored workflow: a skill to invoke plus the argument string
/// passed to it.
///
/// Serde-compatible with the daemon's `WorkflowStep` (`{ skill, args }`). The
/// daemon treats `args` as `Option<String>`; here it is always `Some`-able via
/// the non-empty `args` string, and serialises as a plain `args = "..."` field.
/// When `args` is empty the field is skipped so the on-disk form stays clean
/// (`[[workflows.steps]] skill = "x"`), matching the daemon's
/// `skip_serializing_if = "Option::is_none"` behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStep {
    /// Stable identifier of this step within its workflow. Assigned during
    /// authoring as the step's zero-based position in the original `steps`
    /// vector, and referenced by other steps' [`depends_on`](WorkflowStep::depends_on).
    ///
    /// It is serialised so the dependency graph survives a `to_toml` →
    /// parse round-trip. The daemon (which keys steps positionally) ignores it.
    #[serde(default)]
    pub id: StepId,
    /// Fully-qualified skill name, exactly as it appears in the [`SkillCatalog`]
    /// (e.g. `"frontend-design:frontend-design"` or a bare `"impeccable"`).
    pub skill: String,
    /// Argument string passed to the skill when invoked. Empty means "no args"
    /// and is omitted from the serialised TOML.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub args: String,
    /// Ids of the steps that must complete before this one may run.
    ///
    /// Assigned by *phase layer* during authoring: every step in phase `P`
    /// depends on **all** steps of the immediately-preceding non-empty phase,
    /// and on no step within its own phase — so same-phase steps are
    /// parallelizable. Empty for steps in the first non-empty phase. Omitted
    /// from the serialised TOML when empty to keep the document clean.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<StepId>,
}

/// Stable identifier of a [`WorkflowStep`] within its workflow.
///
/// A transparent newtype over the step's zero-based position in the authored
/// `steps` vector. Used both as a step's own [`id`](WorkflowStep::id) and inside
/// other steps' [`depends_on`](WorkflowStep::depends_on) edges.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StepId(pub usize);

impl StepId {
    /// The underlying index value.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0
    }
}

impl From<usize> for StepId {
    fn from(v: usize) -> Self {
        Self(v)
    }
}

impl WorkflowStep {
    /// Construct a step from any string-like skill name and args, with id `0`
    /// and no dependencies. Authoring overwrites `id`/`depends_on`; tests and
    /// the daemon mapping can set them explicitly via the public fields.
    pub fn new(skill: impl Into<String>, args: impl Into<String>) -> Self {
        Self {
            id: StepId(0),
            skill: skill.into(),
            args: args.into(),
            depends_on: Vec::new(),
        }
    }
}

/// A complete authored workflow: a named, described, ordered list of steps.
///
/// Serde-compatible with the daemon's `Workflow` (`{ name, description, steps }`).
/// Wrapping it in a single-element `workflows` array and adding
/// `schema_version` (which [`to_toml`](WorkflowSpec::to_toml) does) yields the
/// exact `WorkflowsFile` document the daemon's `load_from` parses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowSpec {
    /// Slug-style workflow name. Used as the `{workflow:<name>}` activation key.
    pub name: String,
    /// Human-readable one-line summary (the originating goal, normalised).
    pub description: String,
    /// Ordered steps. Never empty for a successfully-authored workflow.
    pub steps: Vec<WorkflowStep>,
}

/// Errors raised while authoring or validating a workflow.
#[allow(clippy::module_name_repetitions)] // `WorkflowGenError` is the documented public error name
#[derive(Debug, Error, PartialEq, Eq)]
pub enum WorkflowGenError {
    /// The goal was empty or contained no usable word tokens.
    #[error("goal is empty or has no usable tokens")]
    EmptyGoal,
    /// The catalog had no skills, so no step could ever be authored.
    #[error("skill catalog is empty")]
    EmptyCatalog,
    /// No catalog skill scored above zero for the goal — nothing to author.
    #[error("no catalog skill matches goal {0:?}")]
    NoMatch(String),
    /// A step referenced a skill absent from the catalog (validation failure).
    #[error("step {index} references unknown skill {skill:?}")]
    UnknownSkill {
        /// Zero-based index of the offending step.
        index: usize,
        /// The unknown skill name.
        skill: String,
    },
    /// Serialising the spec to TOML failed.
    #[error("toml serialize: {0}")]
    Serialize(String),
    /// A step's `depends_on` referenced an id not present in the workflow.
    #[error("step {from} depends on unknown step id {to}")]
    UnknownDependency {
        /// Id of the step carrying the dangling edge.
        from: usize,
        /// The referenced id that does not exist in the workflow.
        to: usize,
    },
    /// The dependency graph contains a cycle, so no topological layering exists.
    #[error("workflow dependency graph has a cycle among step ids {0:?}")]
    CyclicDependency(Vec<usize>),
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, WorkflowGenError>;

/// A single skill the agent has available: its name and a short description.
///
/// This is the unit the planner matches against. It is intentionally tiny and
/// owned by this crate so the authoring layer has no dependency on
/// `origin-skills` or the daemon — Phase 2 adapts whatever skill registry it
/// has into a [`SkillCatalog`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInfo {
    /// Fully-qualified skill name (matches the daemon's `Skill` tool `name`).
    pub name: String,
    /// One-line description used for keyword matching.
    pub description: String,
}

impl SkillInfo {
    /// Construct a [`SkillInfo`] from string-likes.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
        }
    }
}

/// The set of skills available for authoring, in a stable order.
///
/// Order matters for determinism: equal-scoring skills tie-break by their index
/// here, so the same catalog always yields the same plan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillCatalog {
    /// Available skills in priority/registration order.
    pub skills: Vec<SkillInfo>,
}

impl SkillCatalog {
    /// Build a catalog from a list of [`SkillInfo`].
    #[must_use]
    pub const fn new(skills: Vec<SkillInfo>) -> Self {
        Self { skills }
    }

    /// `true` when the catalog has no skills.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Number of skills in the catalog.
    #[must_use]
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// `true` when a skill with exactly `name` exists.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.skills.iter().any(|s| s.name == name)
    }
}

/// The canonical lifecycle phases the planner decomposes a goal into.
///
/// Ordering of the enum *is* the pipeline order: a workflow always lists active
/// phases Explore → Plan → Implement → Verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    /// Investigate / understand the existing system.
    Explore,
    /// Design / plan the approach before writing code.
    Plan,
    /// Build / write / edit the change.
    Implement,
    /// Test / verify / review the result.
    Verify,
}

impl Phase {
    /// All phases in pipeline order.
    const ALL: [Self; 4] = [Self::Explore, Self::Plan, Self::Implement, Self::Verify];

    /// Stable lower-case label used in generated step args / names.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Explore => "explore",
            Self::Plan => "plan",
            Self::Implement => "implement",
            Self::Verify => "verify",
        }
    }

    /// Trigger lexemes: if any appears in the goal tokens the phase is
    /// explicitly requested. These also seed skill scoring for the phase.
    const fn lexemes(self) -> &'static [&'static str] {
        match self {
            Self::Explore => &[
                "explore",
                "understand",
                "investigate",
                "research",
                "analyze",
                "analyse",
                "read",
                "scout",
                "map",
                "find",
                "discover",
                "study",
                "examine",
                "audit",
            ],
            Self::Plan => &[
                "plan",
                "design",
                "architect",
                "brainstorm",
                "shape",
                "spec",
                "strategy",
                "outline",
                "scope",
                "approach",
            ],
            Self::Implement => &[
                "implement",
                "build",
                "write",
                "create",
                "add",
                "make",
                "develop",
                "code",
                "fix",
                "refactor",
                "feature",
                "edit",
                "change",
                "generate",
            ],
            Self::Verify => &[
                "verify", "test", "check", "validate", "review", "lint", "ensure", "confirm", "audit", "qa",
            ],
        }
    }
}

/// Tokenize free text into lower-case alphanumeric word tokens.
///
/// Splits on any non-alphanumeric character, lower-cases, and drops empty
/// fragments. Pure and allocation-bounded.
#[must_use]
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Very small stop-word set so common filler words don't dominate scoring.
const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "to", "of", "in", "on", "for", "with", "then", "this", "that", "it", "is",
    "are", "be", "by", "at", "as", "into", "from", "my", "our", "your", "all", "any", "so", "we", "i",
];

fn is_stop_word(tok: &str) -> bool {
    STOP_WORDS.contains(&tok)
}

/// Score how well a skill matches a set of query tokens.
///
/// The score is the count of *distinct* query tokens that appear in the skill's
/// name or description tokens, with name matches weighted double (a token in the
/// skill name is a stronger signal of intent than one buried in prose). A
/// substring match of a query token inside a name token (e.g. query `test`
/// inside name `pretest`) counts as a half-weight partial hit so near-misses
/// still rank above unrelated skills.
///
/// Pure: depends only on its arguments. Determinism comes from the caller
/// iterating the catalog in index order and keeping the first max.
#[must_use]
pub fn score_skill(skill: &SkillInfo, query_tokens: &[String]) -> u32 {
    let name_tokens = tokenize(&skill.name);
    let desc_tokens = tokenize(&skill.description);
    let mut score: u32 = 0;
    let mut seen: Vec<&str> = Vec::new();
    for q in query_tokens {
        if is_stop_word(q) {
            continue;
        }
        // De-dup query tokens so a repeated word can't inflate the score.
        if seen.contains(&q.as_str()) {
            continue;
        }
        seen.push(q.as_str());

        if name_tokens.iter().any(|n| n == q) {
            score = score.saturating_add(4);
        } else if desc_tokens.iter().any(|d| d == q) {
            score = score.saturating_add(2);
        } else if name_tokens
            .iter()
            .any(|n| n.contains(q.as_str()) || q.contains(n.as_str()))
        {
            // partial / substring hit on the name
            score = score.saturating_add(1);
        }
    }
    score
}

/// Pick the best-matching catalog skill for a query, returning its index.
///
/// Returns `None` if the catalog is empty or no skill scores above zero.
/// Ties break toward the lowest catalog index (registration priority), which is
/// what makes selection deterministic.
fn best_skill(catalog: &SkillCatalog, query_tokens: &[String]) -> Option<(usize, u32)> {
    let mut best: Option<(usize, u32)> = None;
    for (idx, skill) in catalog.skills.iter().enumerate() {
        let s = score_skill(skill, query_tokens);
        if s == 0 {
            continue;
        }
        match best {
            Some((_, bs)) if bs >= s => {}
            _ => best = Some((idx, s)),
        }
    }
    best
}

/// Determine which lifecycle phases the goal explicitly requests.
///
/// A phase is "explicit" when any of its trigger lexemes appears in the goal
/// tokens. Returns phases in canonical pipeline order.
#[must_use]
pub fn detected_phases(goal_tokens: &[String]) -> Vec<Phase> {
    Phase::ALL
        .into_iter()
        .filter(|phase| {
            let lex = phase.lexemes();
            goal_tokens.iter().any(|t| lex.contains(&t.as_str()))
        })
        .collect()
}

/// Slugify a goal into a stable, filename-safe workflow name.
///
/// Lower-cases, replaces runs of non-alphanumerics with a single `-`, trims
/// leading/trailing `-`, and caps length. Empty result falls back to
/// `"workflow"`.
#[must_use]
pub fn slugify(goal: &str, max_tokens: usize) -> String {
    let toks: Vec<String> = tokenize(goal)
        .into_iter()
        .filter(|t| !is_stop_word(t))
        .take(max_tokens.max(1))
        .collect();
    if toks.is_empty() {
        return "workflow".to_string();
    }
    toks.join("-")
}

fn truncate_arg(goal: &str) -> String {
    let trimmed = goal.trim();
    if trimmed.chars().count() <= MAX_ARG_LEN {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(MAX_ARG_LEN).collect();
    // Trim a trailing partial word for tidiness.
    match cut.rsplit_once(char::is_whitespace) {
        Some((head, _)) if !head.is_empty() => head.to_string(),
        _ => cut,
    }
}

/// Author a workflow from a natural-language goal and a skill catalog.
///
/// Deterministic and offline (no model call). See the crate-level docs for the
/// full strategy. The returned [`WorkflowSpec`] is guaranteed to:
/// - have a non-empty `steps` list (for a non-empty goal against a non-empty
///   catalog),
/// - reference only skills present in `catalog`,
/// - be identical for identical `(goal, catalog)` inputs.
///
/// # Errors
/// - [`WorkflowGenError::EmptyGoal`] if `goal` has no usable tokens.
/// - [`WorkflowGenError::EmptyCatalog`] if `catalog` has no skills.
/// - [`WorkflowGenError::NoMatch`] if no catalog skill scores above zero for the
///   goal (e.g. a garbage goal sharing no vocabulary with any skill).
pub fn author_workflow(goal: &str, catalog: &SkillCatalog) -> Result<WorkflowSpec> {
    let goal_tokens = tokenize(goal);
    let meaningful: Vec<String> = goal_tokens.iter().filter(|t| !is_stop_word(t)).cloned().collect();
    if meaningful.is_empty() {
        return Err(WorkflowGenError::EmptyGoal);
    }
    if catalog.is_empty() {
        return Err(WorkflowGenError::EmptyCatalog);
    }

    // Phases the goal explicitly asks for; if none, use the full default
    // pipeline so a bare "add X" goal still gets explore→plan→implement→verify.
    let mut phases = detected_phases(&goal_tokens);
    if phases.is_empty() {
        phases = Phase::ALL.to_vec();
    }

    let arg = truncate_arg(goal);
    let mut steps: Vec<WorkflowStep> = Vec::new();
    let mut used_idx: Vec<usize> = Vec::new();
    // Ids of the steps emitted by the most recent *non-empty* phase. Every step
    // of the next non-empty phase depends on all of these — phase-layered DAG.
    let mut prev_layer: Vec<StepId> = Vec::new();

    for phase in phases {
        // Query = phase lexemes + meaningful goal tokens, so a skill is judged
        // both on phase fit and on goal-topic fit.
        let mut query: Vec<String> = phase.lexemes().iter().map(|s| (*s).to_string()).collect();
        query.extend(meaningful.iter().cloned());

        if let Some((idx, _score)) = best_skill_excluding(catalog, &query, &used_idx) {
            used_idx.push(idx);
            let skill_name = catalog.skills[idx].name.clone();
            let id = StepId(steps.len());
            steps.push(WorkflowStep {
                id,
                skill: skill_name,
                args: arg.clone(),
                // Depend on the whole previous non-empty layer; steps within
                // this same phase do not depend on each other.
                depends_on: prev_layer.clone(),
            });
            // This phase emitted exactly one step; it becomes the layer the next
            // non-empty phase depends on.
            prev_layer = vec![id];
        }
        // Phase with no positive match is silently skipped — we never invent a
        // skill that isn't in the catalog, and an empty phase does not reset the
        // "previous non-empty layer".
    }

    // Guarantee non-empty: if phase matching produced nothing (e.g. skills whose
    // descriptions don't share any lifecycle vocabulary), fall back to the
    // single best whole-goal match.
    if steps.is_empty() {
        match best_skill(catalog, &meaningful) {
            Some((idx, _)) => {
                let skill_name = catalog.skills[idx].name.clone();
                steps.push(WorkflowStep {
                    id: StepId(0),
                    skill: skill_name,
                    args: arg.clone(),
                    depends_on: Vec::new(),
                });
            }
            None => return Err(WorkflowGenError::NoMatch(goal.trim().to_string())),
        }
    }

    let spec = WorkflowSpec {
        name: slugify(goal, 5),
        description: arg,
        steps,
    };
    // Defensive: the construction above only ever uses catalog skills, but
    // validate() makes the invariant explicit and is cheap.
    spec.validate(catalog)?;
    Ok(spec)
}

/// Like [`best_skill`] but skips already-used catalog indices, so a multi-phase
/// pipeline doesn't pick the same skill twice when distinct skills are available.
fn best_skill_excluding(
    catalog: &SkillCatalog,
    query_tokens: &[String],
    used: &[usize],
) -> Option<(usize, u32)> {
    let mut best: Option<(usize, u32)> = None;
    for (idx, skill) in catalog.skills.iter().enumerate() {
        if used.contains(&idx) {
            continue;
        }
        let s = score_skill(skill, query_tokens);
        if s == 0 {
            continue;
        }
        match best {
            Some((_, bs)) if bs >= s => {}
            _ => best = Some((idx, s)),
        }
    }
    best
}

/// Compute the topological *execution layers* of a workflow's dependency graph.
///
/// Layer 0 is every step with no unmet dependencies (its `depends_on` is empty);
/// each subsequent layer is the set of steps whose dependencies are *all*
/// satisfied by strictly-earlier layers. Steps within one layer have no edges
/// between them, so they are safe to run concurrently — this is exactly the
/// phase-layered parallelism `author_workflow` encodes.
///
/// This is Kahn's algorithm grouped by layer: repeatedly emit the set of
/// not-yet-scheduled steps whose every dependency is already scheduled. Within a
/// layer, steps are returned in the order they appear in `spec.steps`, so the
/// output is deterministic.
///
/// # Errors
/// - [`WorkflowGenError::UnknownDependency`] if a step depends on an id that is
///   not present in the workflow.
/// - [`WorkflowGenError::CyclicDependency`] if the graph contains a cycle (no
///   layering exists); the error carries the ids still unscheduled.
pub fn execution_layers(spec: &WorkflowSpec) -> Result<Vec<Vec<StepId>>> {
    // Validate every dependency edge points at a real step id first, so a
    // dangling edge is reported distinctly from a genuine cycle.
    let present: Vec<StepId> = spec.steps.iter().map(|s| s.id).collect();
    for step in &spec.steps {
        for dep in &step.depends_on {
            if !present.contains(dep) {
                return Err(WorkflowGenError::UnknownDependency {
                    from: step.id.index(),
                    to: dep.index(),
                });
            }
        }
    }

    let total = spec.steps.len();
    // `scheduled[i]` == true once `spec.steps[i]` has been placed in a layer.
    let mut scheduled = vec![false; total];
    let mut placed = 0usize;
    let mut layers: Vec<Vec<StepId>> = Vec::new();

    while placed < total {
        // A step is ready when it is not yet scheduled and all of its deps are
        // already scheduled (in some earlier layer).
        let mut layer: Vec<StepId> = Vec::new();
        let mut ready_positions: Vec<usize> = Vec::new();
        for (pos, step) in spec.steps.iter().enumerate() {
            if scheduled[pos] {
                continue;
            }
            let deps_done = step.depends_on.iter().all(|dep| {
                spec.steps
                    .iter()
                    .position(|s| s.id == *dep)
                    .is_some_and(|dpos| scheduled[dpos])
            });
            if deps_done {
                ready_positions.push(pos);
                layer.push(step.id);
            }
        }

        if layer.is_empty() {
            // Nothing became ready yet steps remain -> the unscheduled set forms
            // (or feeds into) a cycle. Report the remaining ids.
            let remaining: Vec<usize> = spec
                .steps
                .iter()
                .enumerate()
                .filter(|(pos, _)| !scheduled[*pos])
                .map(|(_, s)| s.id.index())
                .collect();
            return Err(WorkflowGenError::CyclicDependency(remaining));
        }

        for pos in ready_positions {
            scheduled[pos] = true;
            placed += 1;
        }
        layers.push(layer);
    }

    Ok(layers)
}

impl WorkflowSpec {
    /// Validate that every step references a skill present in `catalog`.
    ///
    /// # Errors
    /// Returns [`WorkflowGenError::UnknownSkill`] for the first step whose skill
    /// is absent from the catalog.
    pub fn validate(&self, catalog: &SkillCatalog) -> Result<()> {
        for (index, step) in self.steps.iter().enumerate() {
            if !catalog.contains(&step.skill) {
                return Err(WorkflowGenError::UnknownSkill {
                    index,
                    skill: step.skill.clone(),
                });
            }
        }
        Ok(())
    }

    /// Serialise this spec into the exact `workflows.toml` document shape the
    /// daemon's `load_from` parses: a `schema_version` plus a single
    /// `[[workflows]]` entry carrying this spec's name/description/steps.
    ///
    /// # Errors
    /// Returns [`WorkflowGenError::Serialize`] if TOML serialisation fails
    /// (should not happen for well-formed UTF-8 string fields).
    pub fn to_toml(&self) -> Result<String> {
        // Build the daemon-shaped document via serde so we don't hand-format
        // (and so it round-trips through the daemon loader unchanged).
        let doc = WorkflowsFileShape {
            schema_version: SCHEMA_VERSION,
            workflows: vec![self.clone()],
        };
        toml::to_string_pretty(&doc).map_err(|e| WorkflowGenError::Serialize(e.to_string()))
    }
}

/// Schema version emitted in generated `workflows.toml` documents. Matches the
/// daemon/cli `SCHEMA_VERSION` (1).
pub const SCHEMA_VERSION: u32 = 1;

/// Internal serde shape mirroring the daemon's `WorkflowsFile`
/// (`{ schema_version, workflows: [WorkflowSpec] }`). Kept private; the public
/// surface is [`WorkflowSpec`] which maps 1:1 onto the daemon's `Workflow`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkflowsFileShape {
    schema_version: u32,
    #[serde(default)]
    workflows: Vec<WorkflowSpec>,
}

/// Author a workflow and render it to TOML in one call.
///
/// Convenience entry for the future `AuthorWorkflow` tool and the
/// `origin workflow author <goal>` CLI: returns both the structured spec (for
/// persistence via the daemon's workflows save path) and the rendered
/// `workflows.toml` text (for display / file write).
///
/// # Errors
/// Forwards any [`WorkflowGenError`] from [`author_workflow`] or
/// [`WorkflowSpec::to_toml`].
pub fn author_and_render(goal: &str, catalog: &SkillCatalog) -> Result<(WorkflowSpec, String)> {
    let spec = author_workflow(goal, catalog)?;
    let toml = spec.to_toml()?;
    Ok((spec, toml))
}

/// JSON-schema describing the input to a future `AuthorWorkflow` builtin tool.
///
/// The tool takes a single required `goal` string (the catalog is supplied by
/// the daemon from its live skill registry, not by the model). Phase 2
/// registers this schema with the tool dispatcher.
#[must_use]
pub fn tool_input_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "goal": {
                "type": "string",
                "description": "Natural-language description of what the workflow should accomplish. The planner decomposes it into an ordered explore/plan/implement/verify pipeline over the available skills."
            },
            "name": {
                "type": "string",
                "description": "Optional explicit workflow name. When omitted a slug is derived from the goal."
            }
        },
        "required": ["goal"],
        "additionalProperties": false
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // Minimal mirror of the daemon's on-disk structs, used to prove the
    // generated TOML parses into the exact `{name, steps:[{skill, args}]}`
    // shape `origin-daemon::workflows::load_from` expects.
    #[derive(Deserialize)]
    struct DaemonStep {
        skill: String,
        #[serde(default)]
        args: Option<String>,
    }
    #[derive(Deserialize)]
    struct DaemonWorkflow {
        name: String,
        #[serde(default)]
        description: Option<String>,
        steps: Vec<DaemonStep>,
    }
    #[derive(Deserialize)]
    struct DaemonFile {
        #[serde(default)]
        schema_version: u32,
        #[serde(default)]
        workflows: Vec<DaemonWorkflow>,
    }

    fn sample_catalog() -> SkillCatalog {
        SkillCatalog::new(vec![
            SkillInfo::new("scout", "explore and understand the existing codebase quickly"),
            SkillInfo::new("brainstorming", "plan and design the approach before coding"),
            SkillInfo::new("impeccable", "implement and build production-grade features"),
            SkillInfo::new("verify", "run tests, lint, and verify the change works"),
            SkillInfo::new("unrelated", "make coffee and water the plants"),
        ])
    }

    #[test]
    fn tokenize_splits_and_lowercases() {
        assert_eq!(
            tokenize("Explore the Auth-Code, then TEST!"),
            vec!["explore", "the", "auth", "code", "then", "test"]
        );
        assert!(tokenize("   ").is_empty());
        assert!(tokenize("!!!@@@").is_empty());
    }

    #[test]
    fn score_weights_name_above_description() {
        let s = SkillInfo::new("verify", "run tests");
        // "verify" matches the name (weight 4); "tests" matches description (2).
        let q = tokenize("verify tests");
        assert_eq!(score_skill(&s, &q), 6);
        // a token present nowhere contributes nothing
        let q2 = tokenize("verify banana");
        assert_eq!(score_skill(&s, &q2), 4);
    }

    #[test]
    fn score_ignores_stop_words_and_dedups() {
        let s = SkillInfo::new("plan", "plan the work");
        // "the" is a stop word; "plan plan" should only count once (name weight 4).
        let q = tokenize("plan the plan");
        assert_eq!(score_skill(&s, &q), 4);
    }

    #[test]
    fn detected_phases_orders_canonically() {
        // Goal mentions verify before explore; output must still be canonical.
        let toks = tokenize("test the thing then explore it");
        assert_eq!(detected_phases(&toks), vec![Phase::Explore, Phase::Verify]);
    }

    #[test]
    fn multi_intent_goal_yields_multiple_ordered_steps() {
        let cat = sample_catalog();
        let spec = author_workflow(
            "explore the auth module, plan the change, implement login, and test it",
            &cat,
        )
        .unwrap();
        // explore -> plan -> implement -> verify, mapped to real skills, in order.
        let skills: Vec<&str> = spec.steps.iter().map(|s| s.skill.as_str()).collect();
        assert_eq!(skills, vec!["scout", "brainstorming", "impeccable", "verify"]);
        // every step carries the goal as args
        assert!(spec.steps.iter().all(|s| s.args.contains("explore the auth")));
        spec.validate(&cat).unwrap();
    }

    #[test]
    fn bare_goal_uses_full_default_pipeline() {
        let cat = sample_catalog();
        // No explicit lifecycle verb beyond "add"/"feature" (Implement). The
        // default pipeline should still produce explore/plan/implement/verify
        // because no *single* phase dominates... actually "add" is Implement.
        // Use a goal with no lifecycle verb at all:
        let spec = author_workflow("dark mode toggle preference", &cat).unwrap();
        // With no phase verbs detected we fall back to the full pipeline, so we
        // should get up to 4 distinct steps, all real skills, in canonical order.
        assert!(!spec.steps.is_empty());
        spec.validate(&cat).unwrap();
        // ordering invariant: indices used are non-decreasing across phases is
        // NOT guaranteed, but skills must be distinct.
        let mut names: Vec<&str> = spec.steps.iter().map(|s| s.skill.as_str()).collect();
        let before = names.clone();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before.len(), "no duplicate skills in pipeline");
    }

    #[test]
    fn empty_goal_is_rejected() {
        let cat = sample_catalog();
        assert_eq!(author_workflow("", &cat), Err(WorkflowGenError::EmptyGoal));
        assert_eq!(author_workflow("   \t\n", &cat), Err(WorkflowGenError::EmptyGoal));
        // pure punctuation tokenizes to nothing
        assert_eq!(
            author_workflow("!!! ??? ...", &cat),
            Err(WorkflowGenError::EmptyGoal)
        );
        // stop-words-only is still empty of meaningful tokens
        assert_eq!(
            author_workflow("the and or to of", &cat),
            Err(WorkflowGenError::EmptyGoal)
        );
    }

    #[test]
    fn empty_catalog_is_rejected() {
        let cat = SkillCatalog::default();
        assert_eq!(
            author_workflow("do something useful", &cat),
            Err(WorkflowGenError::EmptyCatalog)
        );
    }

    #[test]
    fn garbage_goal_with_no_overlap_errors_nomatch() {
        // A catalog whose vocabulary shares nothing with the goal and where the
        // goal triggers no lifecycle phase -> NoMatch.
        let cat = SkillCatalog::new(vec![SkillInfo::new("widget", "frobnicate the gizmos")]);
        let res = author_workflow("xyzzy plugh quux", &cat);
        assert!(matches!(res, Err(WorkflowGenError::NoMatch(_))), "got {res:?}");
    }

    #[test]
    fn no_unknown_skills_ever_emitted() {
        let cat = sample_catalog();
        let spec = author_workflow("implement and verify the parser", &cat).unwrap();
        for step in &spec.steps {
            assert!(cat.contains(&step.skill), "emitted unknown skill {}", step.skill);
        }
    }

    #[test]
    fn validate_rejects_unknown_skill() {
        let cat = sample_catalog();
        let spec = WorkflowSpec {
            name: "x".into(),
            description: "x".into(),
            steps: vec![
                WorkflowStep::new("scout", "g"),
                WorkflowStep::new("does-not-exist", "g"),
            ],
        };
        assert_eq!(
            spec.validate(&cat),
            Err(WorkflowGenError::UnknownSkill {
                index: 1,
                skill: "does-not-exist".into()
            })
        );
    }

    #[test]
    fn determinism_same_input_same_output() {
        let cat = sample_catalog();
        let g = "explore then plan then implement then verify the feature";
        let a = author_workflow(g, &cat).unwrap();
        let b = author_workflow(g, &cat).unwrap();
        assert_eq!(a, b);
        // and the rendered TOML is byte-identical too
        assert_eq!(a.to_toml().unwrap(), b.to_toml().unwrap());
    }

    #[test]
    fn toml_round_trips_into_daemon_shape() {
        let cat = sample_catalog();
        let (spec, toml_text) = author_and_render("explore and implement and test the api", &cat).unwrap();
        // The emitted TOML must parse back into the daemon-shaped document.
        let reparsed: WorkflowsFileShape = toml::from_str(&toml_text).expect("daemon-shape parse");
        assert_eq!(reparsed.schema_version, SCHEMA_VERSION);
        assert_eq!(reparsed.workflows.len(), 1);
        assert_eq!(reparsed.workflows[0], spec);

        // And it matches the literal {name, steps:[{skill, args}]} shape: parse
        // into a minimal mirror of the daemon's structs (defined at module top).
        let daemon: DaemonFile = toml::from_str(&toml_text).expect("daemon struct parse");
        assert_eq!(daemon.schema_version, SCHEMA_VERSION);
        assert_eq!(daemon.workflows.len(), 1);
        let wf = &daemon.workflows[0];
        assert!(!wf.name.is_empty());
        assert!(wf.description.is_some());
        assert!(!wf.steps.is_empty());
        for s in &wf.steps {
            assert!(cat.contains(&s.skill));
            // args present and non-empty for an authored step
            assert!(s.args.as_deref().is_some_and(|a| !a.is_empty()));
        }
    }

    #[test]
    fn empty_args_step_is_skipped_in_toml() {
        // A step with empty args must not emit an `args = ""` line, matching the
        // daemon's skip-when-None on-disk convention.
        let spec = WorkflowSpec {
            name: "x".into(),
            description: "d".into(),
            steps: vec![WorkflowStep::new("scout", "")],
        };
        let t = spec.to_toml().unwrap();
        assert!(!t.contains("args"), "empty args leaked into TOML:\n{t}");
    }

    #[test]
    fn slugify_is_stable_and_safe() {
        assert_eq!(slugify("Explore the Auth Code!", 5), "explore-auth-code");
        assert_eq!(slugify("   ", 5), "workflow");
        assert_eq!(slugify("!!!", 5), "workflow");
        // capped at max_tokens
        assert_eq!(slugify("one two three four five six seven", 3), "one-two-three");
    }

    #[test]
    fn long_goal_arg_is_truncated() {
        let cat = sample_catalog();
        let long = format!("implement {}", "word ".repeat(200));
        let spec = author_workflow(&long, &cat).unwrap();
        for s in &spec.steps {
            assert!(
                s.args.chars().count() <= MAX_ARG_LEN,
                "arg too long: {}",
                s.args.len()
            );
        }
    }

    #[test]
    fn tool_input_schema_requires_goal() {
        let schema = tool_input_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "goal");
        assert!(schema["properties"]["goal"].is_object());
    }

    #[test]
    fn linear_pipeline_yields_four_singleton_layers() {
        let cat = sample_catalog();
        let spec = author_workflow(
            "explore the auth module, plan the change, implement login, and test it",
            &cat,
        )
        .unwrap();
        // 4 phases -> 4 steps; phase-layered deps make each step depend on the
        // single prior step, so execution_layers is 4 layers of 1 step each.
        assert_eq!(spec.steps.len(), 4);
        let layers = execution_layers(&spec).unwrap();
        assert_eq!(layers.len(), 4, "expected 4 layers, got {layers:?}");
        for layer in &layers {
            assert_eq!(layer.len(), 1, "expected singleton layers, got {layers:?}");
        }
    }

    #[test]
    fn same_phase_steps_share_one_layer() {
        // Phase layout: 1 explore step, then TWO implement steps that both
        // depend only on the explore step and NOT on each other. The two
        // implement steps must land together in a single execution layer,
        // proving they are parallelizable.
        let spec = WorkflowSpec {
            name: "parallel".into(),
            description: "d".into(),
            steps: vec![
                WorkflowStep {
                    id: StepId(0),
                    skill: "scout".into(),
                    args: "g".into(),
                    depends_on: vec![],
                },
                WorkflowStep {
                    id: StepId(1),
                    skill: "impeccable".into(),
                    args: "g".into(),
                    depends_on: vec![StepId(0)],
                },
                WorkflowStep {
                    id: StepId(2),
                    skill: "frontend-design".into(),
                    args: "g".into(),
                    depends_on: vec![StepId(0)],
                },
            ],
        };
        let layers = execution_layers(&spec).unwrap();
        assert_eq!(layers.len(), 2, "expected 2 layers, got {layers:?}");
        assert_eq!(layers[0], vec![StepId(0)]);
        // Both implement steps in the same layer (order = vector order).
        assert_eq!(layers[1], vec![StepId(1), StepId(2)]);
    }

    #[test]
    fn depends_on_round_trips_through_toml() {
        let cat = sample_catalog();
        let spec = author_workflow(
            "explore the auth module, plan the change, implement login, and test it",
            &cat,
        )
        .unwrap();
        // Authored 4-phase pipeline => step N depends on step N-1.
        assert_eq!(spec.steps[0].depends_on, Vec::<StepId>::new());
        assert_eq!(spec.steps[1].depends_on, vec![StepId(0)]);
        assert_eq!(spec.steps[2].depends_on, vec![StepId(1)]);
        assert_eq!(spec.steps[3].depends_on, vec![StepId(2)]);

        let toml_text = spec.to_toml().unwrap();
        // The dependency metadata must be present in the serialised form.
        assert!(
            toml_text.contains("depends_on"),
            "depends_on missing:\n{toml_text}"
        );

        // Round-trip back into the same WorkflowSpec, ids/deps intact.
        let reparsed: WorkflowsFileShape = toml::from_str(&toml_text).expect("parse");
        assert_eq!(reparsed.workflows.len(), 1);
        assert_eq!(reparsed.workflows[0], spec);
        // And the recovered graph layers identically.
        assert_eq!(
            execution_layers(&reparsed.workflows[0]).unwrap(),
            execution_layers(&spec).unwrap()
        );
    }

    #[test]
    fn cyclic_spec_errors() {
        // Two steps that depend on each other: 0 -> 1 -> 0. No topological
        // layering exists, so execution_layers must return Err(CyclicDependency).
        let spec = WorkflowSpec {
            name: "cycle".into(),
            description: "d".into(),
            steps: vec![
                WorkflowStep {
                    id: StepId(0),
                    skill: "scout".into(),
                    args: "g".into(),
                    depends_on: vec![StepId(1)],
                },
                WorkflowStep {
                    id: StepId(1),
                    skill: "impeccable".into(),
                    args: "g".into(),
                    depends_on: vec![StepId(0)],
                },
            ],
        };
        let res = execution_layers(&spec);
        assert!(
            matches!(&res, Err(WorkflowGenError::CyclicDependency(_))),
            "expected CyclicDependency, got {res:?}"
        );
        if let Err(WorkflowGenError::CyclicDependency(ids)) = res {
            assert!(ids.contains(&0) && ids.contains(&1), "unexpected ids {ids:?}");
        }
    }

    #[test]
    fn dangling_dependency_errors() {
        // A step depends on an id that no step carries -> UnknownDependency,
        // reported distinctly from a genuine cycle.
        let spec = WorkflowSpec {
            name: "dangling".into(),
            description: "d".into(),
            steps: vec![WorkflowStep {
                id: StepId(0),
                skill: "scout".into(),
                args: "g".into(),
                depends_on: vec![StepId::from(99usize)],
            }],
        };
        assert_eq!(
            execution_layers(&spec),
            Err(WorkflowGenError::UnknownDependency { from: 0, to: 99 })
        );
    }

    #[test]
    fn step_id_index_and_from() {
        let id = StepId::from(7usize);
        assert_eq!(id.index(), 7);
        assert_eq!(StepId(0).index(), 0);
        // Default is StepId(0).
        assert_eq!(StepId::default(), StepId(0));
    }

    #[test]
    fn empty_workflow_layers_is_empty() {
        let spec = WorkflowSpec {
            name: "empty".into(),
            description: "d".into(),
            steps: vec![],
        };
        assert_eq!(execution_layers(&spec).unwrap(), Vec::<Vec<StepId>>::new());
    }

    #[test]
    fn catalog_helpers() {
        let cat = sample_catalog();
        assert!(!cat.is_empty());
        assert_eq!(cat.len(), 5);
        assert!(cat.contains("scout"));
        assert!(!cat.contains("nope"));
        assert!(SkillCatalog::default().is_empty());
    }
}
