// SPDX-License-Identifier: Apache-2.0
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

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Current file schema. Bumped when the on-disk shape changes in a way
/// that requires migration; matches the pattern from [`crate::config`].
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStep {
    /// Fully-qualified skill name, e.g. `"frontend-design:frontend-design"`
    /// or a bare `"impeccable"` for top-level skills.
    pub skill: String,
    /// Optional argument string passed to the skill when invoked.
    /// Matches the `args` parameter of the `Skill` tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workflow {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub steps: Vec<WorkflowStep>,
}

/// Top-level on-disk shape of `~/.origin/workflows.toml`.
#[allow(clippy::module_name_repetitions)] // `WorkflowsFile` is the documented public type
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowsFile {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default, rename = "workflows")]
    pub workflows: Vec<Workflow>,
}

const fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

#[allow(clippy::module_name_repetitions)] // `WorkflowsError` is the public error name
#[derive(Debug, Error)]
pub enum WorkflowsError {
    #[error("home directory not found (set $ORIGIN_HOME or $HOME)")]
    NoHome,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("toml serialize: {0}")]
    Serialize(#[from] toml::ser::Error),
}

/// Resolve `~/.origin/workflows.toml`. Honors `$ORIGIN_HOME` for tests and
/// alternate-root installs.
///
/// # Errors
/// Returns [`WorkflowsError::NoHome`] if neither `$ORIGIN_HOME` nor a home
/// directory can be resolved.
pub fn path() -> Result<PathBuf, WorkflowsError> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or(WorkflowsError::NoHome)?;
    Ok(home.join(".origin").join("workflows.toml"))
}

/// `true` when `workflows.toml` is present on disk.
#[must_use]
pub fn exists() -> bool {
    path().map(|p| p.exists()).unwrap_or(false)
}

/// Atomic save: write to `.tmp` sibling, then rename — same convention
/// as `crate::config::save_to`.
///
/// # Errors
/// Returns [`WorkflowsError::Io`] on directory create / write / rename
/// failure or [`WorkflowsError::Serialize`] if `file` fails to serialise.
pub fn save_to(p: &Path, file: &WorkflowsFile) -> Result<(), WorkflowsError> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = toml::to_string_pretty(file)?;
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}

/// Load `workflows.toml` from `p`, returning `Ok(None)` when the file is
/// absent so callers can distinguish first-run from corruption.
///
/// # Errors
/// Returns [`WorkflowsError::Io`] on read failure or
/// [`WorkflowsError::Parse`] on malformed TOML.
pub fn load_from(p: &Path) -> Result<Option<WorkflowsFile>, WorkflowsError> {
    if !p.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(p)?;
    Ok(Some(toml::from_str(&raw)?))
}

/// An example workflow used during onboarding to demonstrate the format.
///
/// Chains `frontend-design:frontend-design` (shape the UX) with
/// `impeccable` invoked as `teach` (set up project design context) —
/// matches the example the operator gave during setup.
#[must_use]
pub fn example_workflow() -> Workflow {
    Workflow {
        name: "frontend-design".to_string(),
        description: Some(
            "Two-step UI feature build: shape with frontend-design, then teach impeccable.".into(),
        ),
        steps: vec![
            WorkflowStep {
                skill: "frontend-design:frontend-design".into(),
                args: None,
            },
            WorkflowStep {
                skill: "impeccable".into(),
                args: Some("teach".into()),
            },
        ],
    }
}

/// Write a starter `workflows.toml` containing one example workflow.
/// Returns `Ok(false)` if a file already exists at `p` (so re-running
/// onboarding doesn't clobber user edits).
///
/// # Errors
/// Forwards [`WorkflowsError`] from [`save_to`].
pub fn seed_if_missing(p: &Path) -> Result<bool, WorkflowsError> {
    if p.exists() {
        return Ok(false);
    }
    let file = WorkflowsFile {
        schema_version: SCHEMA_VERSION,
        workflows: vec![example_workflow()],
    };
    save_to(p, &file)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_example_workflow() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("workflows.toml");
        let file = WorkflowsFile {
            schema_version: SCHEMA_VERSION,
            workflows: vec![example_workflow()],
        };
        save_to(&p, &file).expect("save");
        let loaded = load_from(&p).expect("load").expect("present");
        assert_eq!(file, loaded);
    }

    #[test]
    fn seed_writes_when_missing_no_op_when_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("workflows.toml");
        assert!(seed_if_missing(&p).expect("first seed"));
        assert!(p.exists());
        // Second call must NOT overwrite — verify by editing then re-seeding.
        std::fs::write(&p, "schema_version = 1\n").expect("overwrite to single line");
        assert!(!seed_if_missing(&p).expect("second seed"));
        let raw = std::fs::read_to_string(&p).expect("read");
        assert_eq!(raw.trim(), "schema_version = 1");
    }

    #[test]
    fn load_missing_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("missing.toml");
        assert!(load_from(&p).expect("load").is_none());
    }

    #[test]
    fn skips_args_serialization_when_none() {
        // Confirms the on-disk format stays clean for step entries without
        // an args string — `[[workflows.steps]] skill = "x"` rather than
        // `args = ""`.
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("clean.toml");
        let file = WorkflowsFile {
            schema_version: SCHEMA_VERSION,
            workflows: vec![Workflow {
                name: "x".into(),
                description: None,
                steps: vec![WorkflowStep {
                    skill: "y".into(),
                    args: None,
                }],
            }],
        };
        save_to(&p, &file).expect("save");
        let raw = std::fs::read_to_string(&p).expect("read");
        assert!(!raw.contains("args"), "args field leaked despite skip:\n{raw}");
    }
}
