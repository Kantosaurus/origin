// SPDX-License-Identifier: Apache-2.0
//! Daemon-side workflow loader.
//!
//! Mirrors the on-disk shape produced by
//! `origin init` (see `crates/origin-cli/src/workflows.rs`). Kept as a
//! small duplicate rather than introducing a daemon→cli dep; consolidating
//! into an `origin-workflows` crate is a follow-up.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Current `workflows.toml` schema version. Matches the CLI's
/// `crate::workflows::SCHEMA_VERSION` (1); emitted when this loader saves a
/// file the CLI/daemon then read back.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStep {
    /// Stable identifier of this step within its workflow. A zero-based index
    /// referenced by other steps' [`depends_on`](WorkflowStep::depends_on).
    ///
    /// Carries the phase-layered DAG authored by `origin_workflowgen` from
    /// author time to run time so [`crate::workflow_runner`] can fan steps out
    /// in dependency order. Serde-default `0` so a pre-DAG `workflows.toml` (no
    /// `id` field) still parses; the linear skill-mask sequencer ignores it.
    #[serde(default)]
    pub id: usize,
    pub skill: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<String>,
    /// Ids of the steps that must complete before this one may run. Empty for
    /// steps in the first layer. Omitted from the serialised TOML when empty so
    /// pre-DAG files round-trip byte-identically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub steps: Vec<WorkflowStep>,
}

// name appears in user-facing config error messages, must be unambiguous.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkflowsFile {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub workflows: Vec<Workflow>,
}

/// Load a `workflows.toml` file from disk into a [`WorkflowsFile`].
///
/// Returns an empty `WorkflowsFile` if `p` does not exist.
///
/// # Errors
///
/// Returns an [`std::io::Error`] if the file exists but cannot be read,
/// or if its contents are not valid TOML matching the [`WorkflowsFile`]
/// schema (wrapped as `ErrorKind::InvalidData`).
pub fn load_from(p: &Path) -> std::io::Result<WorkflowsFile> {
    if !p.exists() {
        return Ok(WorkflowsFile::default());
    }
    let raw = std::fs::read_to_string(p)?;
    toml::from_str(&raw).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Resolve `~/.origin/workflows.toml`.
///
/// Honors `$ORIGIN_HOME` for tests and alternate-root installs — identical
/// resolution to the CLI's `crate::workflows::path`, so both surfaces read and
/// write the same file.
///
/// # Errors
/// Returns [`std::io::Error`] of kind [`std::io::ErrorKind::NotFound`] when
/// neither `$ORIGIN_HOME` nor a home directory can be resolved.
pub fn path() -> std::io::Result<PathBuf> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "home directory not found (set $ORIGIN_HOME or $HOME)",
            )
        })?;
    Ok(home.join(".origin").join("workflows.toml"))
}

/// Atomically save `file` to `p`.
///
/// Serialises `file`, writes to a `.tmp` sibling, then renames into place — the
/// same write-then-rename convention the CLI's `save_to` uses, so a crash
/// mid-write never leaves a half-written `workflows.toml`.
///
/// # Errors
/// Returns [`std::io::Error`] on directory create / serialise / write / rename
/// failure (serialisation errors are wrapped as
/// [`std::io::ErrorKind::InvalidData`]).
pub fn save_to(p: &Path, file: &WorkflowsFile) -> std::io::Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = toml::to_string_pretty(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_empty_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("workflows.toml");
        let file = load_from(&p).expect("load");
        assert!(file.workflows.is_empty());
    }

    #[test]
    fn save_to_then_load_from_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("workflows.toml");
        let file = WorkflowsFile {
            schema_version: SCHEMA_VERSION,
            workflows: vec![Workflow {
                name: "explore-impl".into(),
                description: Some("explore then implement".into()),
                steps: vec![
                    WorkflowStep {
                        id: 0,
                        skill: "scout".into(),
                        args: Some("look around".into()),
                        depends_on: Vec::new(),
                    },
                    WorkflowStep {
                        id: 1,
                        skill: "impeccable".into(),
                        args: None,
                        depends_on: vec![0],
                    },
                ],
            }],
        };
        save_to(&p, &file).expect("save");
        let loaded = load_from(&p).expect("load");
        assert_eq!(loaded.schema_version, SCHEMA_VERSION);
        assert_eq!(loaded.workflows.len(), 1);
        assert_eq!(loaded.workflows[0].name, "explore-impl");
        assert_eq!(loaded.workflows[0].steps.len(), 2);
        assert_eq!(loaded.workflows[0].steps[0].args.as_deref(), Some("look around"));
        assert!(loaded.workflows[0].steps[1].args.is_none());
        // The phase-layered DAG (id + depends_on) survives the save→load round
        // trip so `workflow_runner` can fan out in dependency order.
        assert_eq!(loaded.workflows[0].steps[0].id, 0);
        assert!(loaded.workflows[0].steps[0].depends_on.is_empty());
        assert_eq!(loaded.workflows[0].steps[1].id, 1);
        assert_eq!(loaded.workflows[0].steps[1].depends_on, vec![0]);
        // None args / description must not leak an empty field into the file.
        let raw = std::fs::read_to_string(&p).expect("read back");
        assert!(raw.contains("skill = \"impeccable\""), "missing step:\n{raw}");
    }

    #[test]
    fn loads_seeded_example() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("workflows.toml");
        std::fs::write(
            &p,
            "schema_version = 1\n\
             [[workflows]]\n\
             name = \"frontend-design\"\n\
             [[workflows.steps]]\n\
             skill = \"frontend-design:frontend-design\"\n\
             [[workflows.steps]]\n\
             skill = \"impeccable\"\n\
             args = \"teach\"\n",
        )
        .expect("write");
        let file = load_from(&p).expect("load");
        assert_eq!(file.workflows.len(), 1);
        assert_eq!(file.workflows[0].name, "frontend-design");
        assert_eq!(file.workflows[0].steps.len(), 2);
        assert_eq!(file.workflows[0].steps[1].args.as_deref(), Some("teach"));
    }
}
