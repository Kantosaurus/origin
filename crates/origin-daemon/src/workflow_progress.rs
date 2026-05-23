//! Step-by-step workflow execution state. Tracks which step of a
//! user-defined workflow is currently in effect so the daemon can gate
//! activation of the next step on completion of the current one's turn.
//!
//! Pure state machine: no tokio, no IPC. The daemon's connection loop
//! owns one `Option<WorkflowProgress>` per connection and calls `start`
//! on `ActivateWorkflow` + `advance` after each successful `Prompt`.

use crate::skill_catalog::SkillCatalog;
use crate::workflows::{Workflow, WorkflowStep};
use origin_skills::frontmatter::SkillFrontmatter;

/// Per-connection state for a workflow currently being walked one step
/// at a time. `current_skill` is the name of the skill activated for the
/// step now in effect; `next_index` points at the next step in
/// `workflow.steps` to consider on the following `advance`.
#[derive(Debug, Clone)]
pub struct WorkflowProgress {
    pub name: String,
    pub total_steps: usize,
    pub current_step_index: usize,
    pub current_skill: String,
    next_index: usize,
    steps: Vec<WorkflowStep>,
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
    /// No more resolvable steps. `previous_skill` is the trailing skill
    /// the caller must deactivate; `skipped` lists any trailing step
    /// skills walked past with no catalog match.
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
    /// until either a catalog-resolvable step is found or the chain is
    /// exhausted.
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
                .map(|s| WorkflowStep {
                    skill: (*s).into(),
                    args: None,
                })
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
        match WorkflowProgress::start(&workflow, &cat) {
            StartOutcome::Stepped {
                progress,
                front,
                skipped,
            } => {
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
        match WorkflowProgress::start(&workflow, &cat) {
            StartOutcome::Stepped {
                progress,
                front,
                skipped,
            } => {
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
        match WorkflowProgress::start(&workflow, &cat) {
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
        let StartOutcome::Stepped { mut progress, .. } = WorkflowProgress::start(&workflow, &cat)
        else {
            panic!("start");
        };
        match progress.advance(&cat) {
            AdvanceOutcome::Stepped {
                previous_skill,
                front,
                skipped,
            } => {
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
        let StartOutcome::Stepped { mut progress, .. } = WorkflowProgress::start(&workflow, &cat)
        else {
            panic!("start");
        };
        match progress.advance(&cat) {
            AdvanceOutcome::Complete {
                previous_skill,
                skipped,
            } => {
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
        let StartOutcome::Stepped { mut progress, .. } = WorkflowProgress::start(&workflow, &cat)
        else {
            panic!("start");
        };
        match progress.advance(&cat) {
            AdvanceOutcome::Stepped {
                front, skipped, ..
            } => {
                assert_eq!(front.name, "gamma");
                assert_eq!(skipped, vec!["ghost"]);
            }
            other => panic!("expected Stepped, got {other:?}"),
        }
    }
}
