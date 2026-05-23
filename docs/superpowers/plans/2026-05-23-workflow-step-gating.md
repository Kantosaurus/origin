# Workflow Step Gating Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `ActivateWorkflow`'s simultaneous "push every step's skill onto the stack at once" behavior with sequential one-step-at-a-time activation gated on prompt completion — so a workflow's step N+1 only goes active after step N's prompt turn finishes.

**Architecture:** Extract a pure state-machine module (`workflow_progress.rs`) that decides which step is active next, with deterministic unit tests. Plumb a per-connection `Arc<Mutex<Option<WorkflowProgress>>>` through the IPC loop. `ActivateWorkflow` initializes the state machine and activates only the first resolvable step. After each successful `Prompt`, the per-connection loop calls `advance()`; this deactivates the prior step's skill and activates the next, until exhaustion. New wire events `WorkflowStepActive` and `WorkflowComplete` replace the old all-at-once `WorkflowActive`.

**Tech Stack:** Rust 1.83 (workspace MSRV), tokio mutex, serde JSON over IPC, existing `origin-skills` registry/catalog.

---

## Files

- **Create:** `crates/origin-daemon/src/workflow_progress.rs` — pure state machine over a loaded `Workflow` + `SkillCatalog`. No tokio, no IPC.
- **Modify:** `crates/origin-daemon/src/lib.rs` — export the new module.
- **Modify:** `crates/origin-daemon/src/protocol.rs` — add `StreamEvent::WorkflowStepActive` + `StreamEvent::WorkflowComplete`; keep `WorkflowActive` as the "no resolvable steps" terminal frame for back-compat (it's the only path that still emits it).
- **Modify:** `crates/origin-daemon/src/main.rs` — replace the all-at-once activation arm; thread a per-connection `Arc<Mutex<Option<WorkflowProgress>>>`; add an `advance_workflow` helper invoked after each successful `Prompt`.
- **Modify:** `crates/origin-daemon/tests/skill_activation_protocol.rs` — update wire round-trip tests for the new variants.
- **Modify:** `crates/origin-cli/src/main.rs` — render the new events.
- **Modify:** `crates/origin-cli/src/workflows.rs` — drop the "execution semantics out of scope" disclaimer in the module doc, since execution semantics now exist.
- **Modify:** `docs/why-origin.md` — update the "young" section to remove the "no inter-step gating yet" bullet; update the capability table.

---

## Task 1: Pure state machine module

**Files:**
- Create: `crates/origin-daemon/src/workflow_progress.rs`
- Modify: `crates/origin-daemon/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/origin-daemon/src/workflow_progress.rs` with only the test module first:

```rust
//! Step-by-step workflow execution state. Tracks which step of a
//! user-defined workflow is currently in effect so the daemon can gate
//! activation of the next step on completion of the current one's turn.
//!
//! Pure state machine: no tokio, no IPC. The daemon's connection loop
//! owns one `Option<WorkflowProgress>` per connection and calls `start`
//! on `ActivateWorkflow` + `advance` after each successful `Prompt`.

use crate::workflows::Workflow;
use crate::skill_catalog::SkillCatalog;
use origin_skills::frontmatter::SkillFrontmatter;

/// Per-connection state for a workflow currently being walked one step
/// at a time. `current_skill` is the name of the skill activated for the
/// step now in effect; `next_index` points at the next step in
/// `workflow.steps` to consider.
#[derive(Debug, Clone)]
pub struct WorkflowProgress {
    pub name: String,
    pub total_steps: usize,
    pub current_step_index: usize,
    pub current_skill: String,
    next_index: usize,
    steps: Vec<crate::workflows::WorkflowStep>,
}

/// Outcome of starting a workflow.
#[derive(Debug)]
pub enum StartOutcome {
    /// At least one step resolved. `skipped` lists step skills that came
    /// before `front` in the chain but had no catalog match.
    Stepped {
        progress: WorkflowProgress,
        front: SkillFrontmatter,
        skipped: Vec<String>,
    },
    /// No step in the workflow had a catalog match. Caller should treat
    /// this like the existing "nothing activated" frame and surface the
    /// `skipped` list.
    NoResolvableSteps { skipped: Vec<String> },
}

/// Outcome of advancing past the current step.
#[derive(Debug)]
pub enum AdvanceOutcome {
    /// Another step resolved. `previous_skill` is the skill that was in
    /// effect before this advance (caller deactivates it); `front` is
    /// the new active skill (caller activates it); `skipped` lists step
    /// skills walked past during this advance with no catalog match.
    Stepped {
        previous_skill: String,
        front: SkillFrontmatter,
        skipped: Vec<String>,
    },
    /// No more resolvable steps. `previous_skill` is the trailing
    /// skill the caller must deactivate; `skipped` lists any trailing
    /// step skills walked past with no catalog match.
    Complete {
        previous_skill: String,
        skipped: Vec<String>,
    },
}

impl WorkflowProgress {
    /// Try to start `workflow`. Walks `workflow.steps` from index 0 and
    /// returns the first that resolves through `catalog`. Steps with no
    /// catalog match are collected into `skipped`.
    #[must_use]
    pub fn start(workflow: &Workflow, catalog: &SkillCatalog) -> StartOutcome {
        let mut skipped: Vec<String> = Vec::new();
        for (i, step) in workflow.steps.iter().enumerate() {
            if let Some(skill) = catalog.find(&step.skill) {
                let progress = Self {
                    name: workflow.name.clone(),
                    total_steps: workflow.steps.len(),
                    current_step_index: i,
                    current_skill: step.skill.clone(),
                    next_index: i + 1,
                    steps: workflow.steps.clone(),
                };
                return StartOutcome::Stepped {
                    progress,
                    front: skill.front.clone(),
                    skipped,
                };
            }
            skipped.push(step.skill.clone());
        }
        StartOutcome::NoResolvableSteps { skipped }
    }

    /// Advance past `current_skill`. Walks forward from `next_index`
    /// until either a catalog-resolvable step is found or the chain
    /// is exhausted.
    pub fn advance(&mut self, catalog: &SkillCatalog) -> AdvanceOutcome {
        let previous = self.current_skill.clone();
        let mut skipped: Vec<String> = Vec::new();
        while self.next_index < self.steps.len() {
            let i = self.next_index;
            self.next_index += 1;
            let step = &self.steps[i];
            if let Some(skill) = catalog.find(&step.skill) {
                self.current_step_index = i;
                self.current_skill = step.skill.clone();
                return AdvanceOutcome::Stepped {
                    previous_skill: previous,
                    front: skill.front.clone(),
                    skipped,
                };
            }
            skipped.push(step.skill.clone());
        }
        AdvanceOutcome::Complete {
            previous_skill: previous,
            skipped,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::WorkflowStep;
    use std::path::Path;

    fn write_skill(dir: &Path, name: &str) {
        let d = dir.join(name);
        std::fs::create_dir_all(&d).expect("mkdir");
        std::fs::write(
            d.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: t\n---\nbody\n"),
        )
        .expect("write");
    }

    fn wf(name: &str, steps: &[&str]) -> Workflow {
        Workflow {
            name: name.into(),
            description: None,
            steps: steps
                .iter()
                .map(|s| WorkflowStep { skill: (*s).into(), args: None })
                .collect(),
        }
    }

    #[test]
    fn start_activates_first_step_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "alpha");
        write_skill(dir.path(), "beta");
        let cat = SkillCatalog::load_from(dir.path()).expect("cat");
        let workflow = wf("two-step", &["alpha", "beta"]);
        let out = WorkflowProgress::start(&workflow, &cat);
        match out {
            StartOutcome::Stepped { progress, front, skipped } => {
                assert_eq!(progress.current_skill, "alpha");
                assert_eq!(progress.current_step_index, 0);
                assert_eq!(progress.total_steps, 2);
                assert_eq!(front.name, "alpha");
                assert!(skipped.is_empty());
            }
            other => panic!("expected Stepped, got {other:?}"),
        }
    }

    #[test]
    fn start_skips_unresolvable_prefix() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "beta");
        let cat = SkillCatalog::load_from(dir.path()).expect("cat");
        let workflow = wf("skip-front", &["ghost", "alpha", "beta"]);
        let out = WorkflowProgress::start(&workflow, &cat);
        match out {
            StartOutcome::Stepped { progress, front, skipped } => {
                assert_eq!(front.name, "beta");
                assert_eq!(progress.current_step_index, 2);
                assert_eq!(progress.current_skill, "beta");
                assert_eq!(skipped, vec!["ghost", "alpha"]);
            }
            other => panic!("expected Stepped, got {other:?}"),
        }
    }

    #[test]
    fn start_with_no_resolvable_steps_returns_skipped_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cat = SkillCatalog::load_from(dir.path()).expect("cat");
        let workflow = wf("all-ghost", &["ghost-a", "ghost-b"]);
        let out = WorkflowProgress::start(&workflow, &cat);
        match out {
            StartOutcome::NoResolvableSteps { skipped } => {
                assert_eq!(skipped, vec!["ghost-a", "ghost-b"]);
            }
            other => panic!("expected NoResolvableSteps, got {other:?}"),
        }
    }

    #[test]
    fn advance_walks_to_next_resolvable_step() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "alpha");
        write_skill(dir.path(), "beta");
        let cat = SkillCatalog::load_from(dir.path()).expect("cat");
        let workflow = wf("a-b", &["alpha", "beta"]);
        let StartOutcome::Stepped { mut progress, .. } = WorkflowProgress::start(&workflow, &cat) else {
            panic!("start");
        };
        match progress.advance(&cat) {
            AdvanceOutcome::Stepped { previous_skill, front, skipped } => {
                assert_eq!(previous_skill, "alpha");
                assert_eq!(front.name, "beta");
                assert!(skipped.is_empty());
                assert_eq!(progress.current_skill, "beta");
                assert_eq!(progress.current_step_index, 1);
            }
            other => panic!("expected Stepped, got {other:?}"),
        }
    }

    #[test]
    fn advance_past_last_returns_complete() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "alpha");
        let cat = SkillCatalog::load_from(dir.path()).expect("cat");
        let workflow = wf("one-step", &["alpha"]);
        let StartOutcome::Stepped { mut progress, .. } = WorkflowProgress::start(&workflow, &cat) else {
            panic!("start");
        };
        match progress.advance(&cat) {
            AdvanceOutcome::Complete { previous_skill, skipped } => {
                assert_eq!(previous_skill, "alpha");
                assert!(skipped.is_empty());
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn advance_skips_ghost_in_middle() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "alpha");
        write_skill(dir.path(), "gamma");
        let cat = SkillCatalog::load_from(dir.path()).expect("cat");
        let workflow = wf("a-g-g", &["alpha", "ghost", "gamma"]);
        let StartOutcome::Stepped { mut progress, .. } = WorkflowProgress::start(&workflow, &cat) else {
            panic!("start");
        };
        match progress.advance(&cat) {
            AdvanceOutcome::Stepped { front, skipped, .. } => {
                assert_eq!(front.name, "gamma");
                assert_eq!(skipped, vec!["ghost"]);
            }
            other => panic!("expected Stepped, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Wire the module into the lib**

Edit `crates/origin-daemon/src/lib.rs`. Add `pub mod workflow_progress;` alongside the existing `pub mod workflows;`.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p origin-daemon --lib workflow_progress::tests`
Expected: all 6 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/origin-daemon/src/workflow_progress.rs crates/origin-daemon/src/lib.rs
git commit -m "feat(daemon): pure state machine for step-by-step workflow execution"
```

---

## Task 2: New protocol events

**Files:**
- Modify: `crates/origin-daemon/src/protocol.rs`
- Modify: `crates/origin-daemon/tests/skill_activation_protocol.rs`

- [ ] **Step 1: Write failing wire round-trip tests**

Replace the `workflow_active_event_round_trips_as_json` test in `crates/origin-daemon/tests/skill_activation_protocol.rs` (lines 85-102) with three tests for the new + retained variants:

```rust
#[test]
fn workflow_step_active_event_round_trips_as_json() {
    let ev = StreamEvent::WorkflowStepActive {
        name: "frontend-design".into(),
        step_index: 0,
        total_steps: 2,
        skill: "frontend-design:frontend-design".into(),
        skipped: vec!["ghost".into()],
    };
    let body = serde_json::to_vec(&ev).expect("encode");
    let decoded: StreamEvent = serde_json::from_slice(&body).expect("decode");
    match decoded {
        StreamEvent::WorkflowStepActive { name, step_index, total_steps, skill, skipped } => {
            assert_eq!(name, "frontend-design");
            assert_eq!(step_index, 0);
            assert_eq!(total_steps, 2);
            assert_eq!(skill, "frontend-design:frontend-design");
            assert_eq!(skipped, vec!["ghost"]);
        }
        other => panic!("expected WorkflowStepActive, got {other:?}"),
    }
}

#[test]
fn workflow_complete_event_round_trips_as_json() {
    let ev = StreamEvent::WorkflowComplete {
        name: "frontend-design".into(),
        skipped: vec!["tail-ghost".into()],
    };
    let body = serde_json::to_vec(&ev).expect("encode");
    let decoded: StreamEvent = serde_json::from_slice(&body).expect("decode");
    match decoded {
        StreamEvent::WorkflowComplete { name, skipped } => {
            assert_eq!(name, "frontend-design");
            assert_eq!(skipped, vec!["tail-ghost"]);
        }
        other => panic!("expected WorkflowComplete, got {other:?}"),
    }
}

#[test]
fn workflow_active_event_still_carries_no_resolved_path() {
    // Retained variant: emitted ONLY when the workflow exists but no step
    // resolves through the catalog. `steps` is always empty in this path.
    let ev = StreamEvent::WorkflowActive {
        name: "all-ghost".into(),
        steps: vec![],
        skipped: vec!["ghost-a".into(), "ghost-b".into()],
    };
    let body = serde_json::to_vec(&ev).expect("encode");
    let decoded: StreamEvent = serde_json::from_slice(&body).expect("decode");
    match decoded {
        StreamEvent::WorkflowActive { name, steps, skipped } => {
            assert_eq!(name, "all-ghost");
            assert!(steps.is_empty());
            assert_eq!(skipped.len(), 2);
        }
        other => panic!("expected WorkflowActive, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test -p origin-daemon --test skill_activation_protocol workflow_step_active`
Expected: COMPILE FAIL — `StreamEvent::WorkflowStepActive` does not exist.

- [ ] **Step 3: Add the new variants to the protocol**

Edit `crates/origin-daemon/src/protocol.rs`. After the existing `WorkflowActive` variant (currently ending at line 265), add:

```rust
    /// Emitted both on initial `ClientMessage::ActivateWorkflow` (for the
    /// first resolvable step) and after each successful `Prompt` while a
    /// workflow is in progress (for the next resolvable step). Step
    /// activation is gated on prompt completion — only one step's skill
    /// is on the stack at a time.
    ///
    /// `step_index` is the 0-based index into the workflow's `steps` of
    /// the step now in effect. `total_steps` is the length of that
    /// vector. `skill` is the catalog name of the active skill.
    /// `skipped` lists any earlier steps walked past during this
    /// transition because they had no catalog match.
    WorkflowStepActive {
        name: String,
        step_index: u32,
        total_steps: u32,
        skill: String,
        #[serde(default)]
        skipped: Vec<String>,
    },
    /// Emitted after the last step's `Prompt` completes. The previous
    /// step's skill has already been deactivated by the daemon when this
    /// fires. `skipped` lists any trailing unresolvable steps walked
    /// past on the way to completion.
    WorkflowComplete {
        name: String,
        #[serde(default)]
        skipped: Vec<String>,
    },
```

Also update the doc-comment on `ClientMessage::ActivateWorkflow` (currently at lines 122-127) to read:

```rust
    /// Walk `name`'s steps in `~/.origin/workflows.toml`, activating the
    /// FIRST resolvable step's skill on this connection's stack. The
    /// daemon replies with [`StreamEvent::WorkflowStepActive`] for the
    /// active step, or [`StreamEvent::WorkflowActive`] (with empty
    /// `steps`) when no step resolves, or [`StreamEvent::SkillError`]
    /// when the workflow name isn't found. Subsequent steps activate
    /// one-at-a-time after each successful `Prompt`.
    ActivateWorkflow { name: String },
```

- [ ] **Step 4: Run the round-trip tests**

Run: `cargo test -p origin-daemon --test skill_activation_protocol`
Expected: all workflow_* tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-daemon/src/protocol.rs crates/origin-daemon/tests/skill_activation_protocol.rs
git commit -m "feat(protocol): add WorkflowStepActive + WorkflowComplete stream events"
```

---

## Task 3: Daemon handler rewires to step-by-step

**Files:**
- Modify: `crates/origin-daemon/src/main.rs`

- [ ] **Step 1: Add the per-connection workflow state and an advance helper**

Inside the `spawn_in(TaskClass::Critical, async move { ... })` closure in `main.rs:609`, just after the `active_skills` declaration (currently line 615), add:

```rust
        let active_workflow: Arc<tokio::sync::Mutex<Option<origin_daemon::workflow_progress::WorkflowProgress>>> =
            Arc::new(tokio::sync::Mutex::new(None));
```

- [ ] **Step 2: Replace the `ActivateWorkflow` arm with one-shot first-step activation**

Replace lines 785-833 (the entire `ClientMessage::ActivateWorkflow { name } => { ... }` arm) with:

```rust
                ClientMessage::ActivateWorkflow { name } => {
                    use origin_daemon::workflow_progress::{StartOutcome, WorkflowProgress};
                    let conn_clone = Arc::clone(&conn);
                    let home = std::env::var_os("ORIGIN_HOME")
                        .map(std::path::PathBuf::from)
                        .or_else(dirs::home_dir)
                        .unwrap_or_else(|| std::path::PathBuf::from("."));
                    let wf_path = home.join(".origin").join("workflows.toml");
                    let file = match origin_daemon::workflows::load_from(&wf_path) {
                        Ok(f) => f,
                        Err(e) => {
                            let ev = StreamEvent::SkillError {
                                message: format!("workflows.toml load: {e}"),
                            };
                            let body = serde_json::to_vec(&ev).unwrap_or_default();
                            let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
                            continue;
                        }
                    };
                    let Some(wf) = file.workflows.iter().find(|w| w.name == name) else {
                        let ev = StreamEvent::SkillError {
                            message: format!("no such workflow: {name}"),
                        };
                        let body = serde_json::to_vec(&ev).unwrap_or_default();
                        let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
                        continue;
                    };
                    // Defensive: if a prior workflow's step is still
                    // active on this connection, drop it before starting
                    // a new one. (User invoked /workflow before the prior
                    // one ran to completion.)
                    {
                        let mut wf_guard = active_workflow.lock().await;
                        if let Some(prev) = wf_guard.take() {
                            active_skills.lock().await.deactivate(&prev.current_skill);
                        }
                    }
                    let ev = match WorkflowProgress::start(wf, skill_catalog.as_ref()) {
                        StartOutcome::Stepped { progress, front, skipped } => {
                            active_skills.lock().await.activate(front);
                            let step_index = progress.current_step_index as u32;
                            let total_steps = progress.total_steps as u32;
                            let skill = progress.current_skill.clone();
                            *active_workflow.lock().await = Some(progress);
                            StreamEvent::WorkflowStepActive {
                                name: name.clone(),
                                step_index,
                                total_steps,
                                skill,
                                skipped,
                            }
                        }
                        StartOutcome::NoResolvableSteps { skipped } => {
                            StreamEvent::WorkflowActive {
                                name: name.clone(),
                                steps: Vec::new(),
                                skipped,
                            }
                        }
                    };
                    let body = serde_json::to_vec(&ev).unwrap_or_default();
                    let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
                }
```

- [ ] **Step 3: Advance the workflow after a successful Prompt**

Find the `ClientMessage::Prompt(req) => { ... }` arm (currently main.rs:649-678). The body calls `handle_request(...)` and breaks on `false`. After the `handle_request` block (still inside this arm, after the `if !handle_request(...) { break; }` block, before the closing `}` of the arm), insert:

```rust
                    // Gate: if a workflow is in progress on this connection,
                    // advance to the next step now that the prompt's turn
                    // finished. `advance_workflow` is a no-op if no workflow
                    // is active.
                    advance_workflow(
                        &conn,
                        Arc::clone(&active_workflow),
                        Arc::clone(&active_skills),
                        Arc::clone(&skill_catalog),
                    )
                    .await;
```

Then add the helper function near the other free functions in `main.rs` (placement: right after `handle_request` ends, before `handle_switch`):

```rust
/// If a workflow is in progress on this connection, advance past the
/// step that just completed. Deactivates the current step's skill,
/// activates the next resolvable step's skill, and emits the
/// corresponding `WorkflowStepActive` or `WorkflowComplete` event.
/// No-op when no workflow is active.
async fn advance_workflow(
    conn: &SharedConnection,
    active_workflow: Arc<tokio::sync::Mutex<Option<origin_daemon::workflow_progress::WorkflowProgress>>>,
    active_skills: Arc<tokio::sync::Mutex<origin_skills::SkillRegistry>>,
    skill_catalog: Arc<origin_daemon::skill_catalog::SkillCatalog>,
) {
    use origin_daemon::workflow_progress::AdvanceOutcome;
    let mut wf_guard = active_workflow.lock().await;
    let Some(progress) = wf_guard.as_mut() else { return };
    let outcome = progress.advance(skill_catalog.as_ref());
    let ev = match outcome {
        AdvanceOutcome::Stepped { previous_skill, front, skipped } => {
            let name = progress.name.clone();
            let step_index = progress.current_step_index as u32;
            let total_steps = progress.total_steps as u32;
            let skill = progress.current_skill.clone();
            let mut skills = active_skills.lock().await;
            skills.deactivate(&previous_skill);
            skills.activate(front);
            drop(skills);
            StreamEvent::WorkflowStepActive {
                name,
                step_index,
                total_steps,
                skill,
                skipped,
            }
        }
        AdvanceOutcome::Complete { previous_skill, skipped } => {
            let name = progress.name.clone();
            active_skills.lock().await.deactivate(&previous_skill);
            *wf_guard = None;
            StreamEvent::WorkflowComplete { name, skipped }
        }
    };
    let body = serde_json::to_vec(&ev).unwrap_or_default();
    let _ = conn.lock().await.write_frame(FrameKind::Event, &body).await;
}
```

- [ ] **Step 4: Build to confirm the daemon still compiles**

Run: `cargo build -p origin-daemon`
Expected: builds clean with no errors.

- [ ] **Step 5: Run the existing daemon tests to confirm no regressions**

Run: `cargo test -p origin-daemon`
Expected: all tests PASS (the protocol round-trip tests from Task 2 plus the workflow_progress unit tests from Task 1 plus all pre-existing tests).

- [ ] **Step 6: Commit**

```bash
git add crates/origin-daemon/src/main.rs
git commit -m "feat(daemon): gate workflow steps on prompt completion (one skill at a time)"
```

---

## Task 4: CLI renders the new events

**Files:**
- Modify: `crates/origin-cli/src/main.rs`
- Modify: `crates/origin-cli/src/workflows.rs`

- [ ] **Step 1: Update the headless CLI event renderer**

Find the `WorkflowActive` arm in `crates/origin-cli/src/main.rs:662-676`. Add two new arms next to it for the new variants:

```rust
        StreamEvent::WorkflowStepActive {
            name,
            step_index,
            total_steps,
            skill,
            skipped,
        } => {
            let pos = step_index + 1;
            let main = format!(
                "workflow `{name}` step {pos}/{total_steps}: `{skill}` active"
            );
            if skipped.is_empty() {
                Ok(main)
            } else {
                Ok(format!("{main}  (skipped: {})", skipped.join(", ")))
            }
        }
        StreamEvent::WorkflowComplete { name, skipped } => {
            if skipped.is_empty() {
                Ok(format!("workflow `{name}` complete"))
            } else {
                Ok(format!(
                    "workflow `{name}` complete  (skipped: {})",
                    skipped.join(", ")
                ))
            }
        }
```

Leave the existing `WorkflowActive` arm intact — it now handles only the "no resolvable steps" terminal frame.

- [ ] **Step 2: Drop the out-of-scope disclaimer from the workflows module doc**

Edit `crates/origin-cli/src/workflows.rs` lines 1-13. Replace the leading doc-comment with:

```rust
//! Workflows: declarative chains of skills the agent runs in sequence.
//!
//! A workflow is a named list of steps; each step names a skill (and
//! optionally arguments to pass when invoking it). The file lives at
//! `~/.origin/workflows.toml` and is read by the daemon when the user
//! invokes a workflow by name. Onboarding seeds it with an example so
//! the file shape is discoverable.
//!
//! This module is the storage and config-shape layer. The daemon owns
//! execution: on `ClientMessage::ActivateWorkflow` it activates the
//! first resolvable step's skill, then advances one step per completed
//! prompt turn (see `crates/origin-daemon/src/workflow_progress.rs`).
```

- [ ] **Step 3: Confirm the CLI builds**

Run: `cargo build -p origin-cli`
Expected: builds clean.

- [ ] **Step 4: Run CLI tests**

Run: `cargo test -p origin-cli`
Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-cli/src/main.rs crates/origin-cli/src/workflows.rs
git commit -m "feat(cli): render WorkflowStepActive / WorkflowComplete events"
```

---

## Task 5: Update docs/why-origin.md

**Files:**
- Modify: `docs/why-origin.md`

- [ ] **Step 1: Remove the "no inter-step gating yet" bullet**

Find the section starting at line 207 ("Where it's genuinely young"). Replace the first bullet (lines 208-213, the one about workflow execution semantics being out of scope) with:

```markdown
- The daemon's `workflows.rs` is a deliberate duplicate of the CLI's; a
  comment flags `origin-workflows` as a follow-up crate.
- Workflow steps can't yet carry their own permission tier or sandbox
  profile — they inherit from the skill they reference.
```

(i.e. delete the gating bullet entirely; the duplicate-loader and per-step-permission bullets remain.)

- [ ] **Step 2: Update the capability table to reflect step gating**

Find the table at lines 197-204. Add a row after the "Partial activation w/ skipped reporting" row:

```markdown
| Step-by-step gating (one skill active per prompt) | ❌ | ❌ | ❌ | ✅ |
```

- [ ] **Step 3: Commit**

```bash
git add docs/why-origin.md
git commit -m "docs: workflow step gating shipped — update why-origin.md"
```

---

## Task 6: Full workspace verification

- [ ] **Step 1: Run the full workspace build**

Run: `cargo build --workspace`
Expected: builds clean.

- [ ] **Step 2: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: all tests PASS. (If a pre-existing flaky test fails unrelated to this change, note it and re-run that test in isolation.)

- [ ] **Step 3: Run clippy gate**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 4: No commit — this task is verification only.**

If anything failed in steps 1-3, fix it before the plan is considered done. Do not paper over a failure with `#[allow]`.

---
