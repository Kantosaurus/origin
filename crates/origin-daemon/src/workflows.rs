// SPDX-License-Identifier: Apache-2.0
//! Daemon-side workflow loader.
//!
//! Mirrors the on-disk shape produced by
//! `origin init` (see `crates/origin-cli/src/workflows.rs`). Kept as a
//! small duplicate rather than introducing a daemon→cli dep; consolidating
//! into an `origin-workflows` crate is a follow-up.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub skill: String,
    #[serde(default)]
    pub args: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub name: String,
    #[serde(default)]
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
