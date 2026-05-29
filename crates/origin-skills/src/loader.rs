// SPDX-License-Identifier: Apache-2.0
//! Walk `~/.origin/skills/<name>/SKILL.md`, parse each, hash the body.

use crate::frontmatter::{parse_frontmatter, FrontmatterError, ParsedSkill};
use std::fs;
use std::path::Path;
use thiserror::Error;

/// 32-byte blake3 hash of the skill body bytes. Two skills with the same body
/// dedupe in CAS regardless of file path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SkillHash(pub [u8; 32]);

/// A loaded skill: parsed frontmatter + body + content hash + source path.
#[derive(Debug, Clone)]
pub struct Skill {
    pub front: crate::frontmatter::SkillFrontmatter,
    pub body: String,
    pub body_hash: SkillHash,
    pub source: std::path::PathBuf,
}

#[allow(clippy::module_name_repetitions)] // unambiguous from outside the crate
#[derive(Debug, Error)]
pub enum LoaderError {
    #[error("io reading {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("frontmatter in {path}: {source}")]
    Frontmatter {
        path: std::path::PathBuf,
        #[source]
        source: FrontmatterError,
    },
}

/// Walk one level into `root` and load every `<dir>/SKILL.md` found.
///
/// Subdirectories that do not contain a `SKILL.md` are silently skipped.
///
/// # Errors
/// Returns [`LoaderError`] if any encountered `SKILL.md` cannot be read or parsed.
pub fn load_skills_dir(root: &Path) -> Result<Vec<Skill>, LoaderError> {
    let mut out = Vec::new();
    let entries = fs::read_dir(root).map_err(|e| LoaderError::Io {
        path: root.to_path_buf(),
        source: e,
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| LoaderError::Io {
            path: root.to_path_buf(),
            source: e,
        })?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let path = dir.join("SKILL.md");
        if !path.exists() {
            continue;
        }
        let raw = fs::read_to_string(&path).map_err(|e| LoaderError::Io {
            path: path.clone(),
            source: e,
        })?;
        let ParsedSkill { front, body } = parse_frontmatter(&raw).map_err(|e| LoaderError::Frontmatter {
            path: path.clone(),
            source: e,
        })?;
        let body_hash = SkillHash(*blake3::hash(body.as_bytes()).as_bytes());
        out.push(Skill {
            front,
            body,
            body_hash,
            source: path,
        });
    }

    Ok(out)
}

/// Load embedded skills plus any user overrides from `user_root`, with user
/// entries taking precedence on name collision. Missing `user_root` is fine —
/// embedded skills are still returned.
///
/// # Errors
/// Forwards [`LoaderError`] from `load_skills_dir` when `user_root` exists but
/// a file under it is malformed.
pub fn load_all(user_root: &std::path::Path) -> Result<Vec<Skill>, LoaderError> {
    let mut acc = crate::embedded::load_embedded();
    if user_root.exists() {
        let user = load_skills_dir(user_root)?;
        // Index embedded by name for O(1) replacement.
        let mut by_name: std::collections::HashMap<String, usize> = acc
            .iter()
            .enumerate()
            .map(|(i, s)| (s.front.name.clone(), i))
            .collect();
        for skill in user {
            if let Some(i) = by_name.get(&skill.front.name).copied() {
                acc[i] = skill;
            } else {
                by_name.insert(skill.front.name.clone(), acc.len());
                acc.push(skill);
            }
        }
    }
    Ok(acc)
}
