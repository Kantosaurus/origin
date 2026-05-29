// SPDX-License-Identifier: Apache-2.0
//! First-run import from `~/.claude/skills/` into `~/.origin/skills/`.
//!
//! N9.6 — dedupe by body hash; defer to the caller-provided `confirm` for
//! per-skill accept/reject. No filesystem writes happen until the closure
//! returns [`ImportDecision::Accept`].

use crate::loader::{load_skills_dir, LoaderError, Skill};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use thiserror::Error;

/// User's per-skill decision returned by the `confirm` closure.
#[allow(clippy::module_name_repetitions)] // unambiguous from outside the crate
#[derive(Debug, Clone, Copy)]
pub enum ImportDecision {
    Accept,
    Reject,
}

#[allow(clippy::module_name_repetitions)] // unambiguous from outside the crate
#[derive(Debug, Default)]
pub struct ImportReport {
    pub imported: usize,
    pub rejected: usize,
    pub skipped_duplicate: usize,
}

#[allow(clippy::module_name_repetitions)] // unambiguous from outside the crate
#[derive(Debug, Error)]
pub enum ImportError {
    #[error("source: {0}")]
    Source(#[from] LoaderError),
    #[error("io writing {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Walk `src` and copy any skill not already present in `dst` by body hash,
/// gated on `confirm`.
///
/// # Errors
/// Returns [`ImportError::Source`] if `src` cannot be read, or
/// [`ImportError::Io`] on `dst` write failure.
#[allow(clippy::module_name_repetitions)] // unambiguous from outside the crate
pub fn first_run_import<F>(src: &Path, dst: &Path, mut confirm: F) -> Result<ImportReport, ImportError>
where
    F: FnMut(&Skill) -> ImportDecision,
{
    fs::create_dir_all(dst).map_err(|e| ImportError::Io {
        path: dst.to_path_buf(),
        source: e,
    })?;

    let existing: HashSet<[u8; 32]> = if dst.exists() {
        load_skills_dir(dst)
            .map_err(ImportError::Source)?
            .into_iter()
            .map(|s| s.body_hash.0)
            .collect()
    } else {
        HashSet::new()
    };

    let candidates = load_skills_dir(src).map_err(ImportError::Source)?;
    let mut report = ImportReport::default();

    for skill in candidates {
        if existing.contains(&skill.body_hash.0) {
            report.skipped_duplicate += 1;
            continue;
        }
        match confirm(&skill) {
            ImportDecision::Reject => {
                report.rejected += 1;
                continue;
            }
            ImportDecision::Accept => {
                let target_dir = dst.join(&skill.front.name);
                fs::create_dir_all(&target_dir).map_err(|e| ImportError::Io {
                    path: target_dir.clone(),
                    source: e,
                })?;
                let bytes = fs::read(&skill.source).map_err(|e| ImportError::Io {
                    path: skill.source.clone(),
                    source: e,
                })?;
                let target = target_dir.join("SKILL.md");
                fs::write(&target, bytes).map_err(|e| ImportError::Io {
                    path: target,
                    source: e,
                })?;
                report.imported += 1;
            }
        }
    }

    Ok(report)
}
