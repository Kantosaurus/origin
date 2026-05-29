// SPDX-License-Identifier: Apache-2.0
//! In-process catalog of skills the daemon loaded from `~/.origin/skills/`.
//!
//! The daemon loads the catalog once at startup and holds it in an `Arc`
//! shared across every connection. Activation state — which subset of
//! these skills is *currently in the stack* — lives separately, per
//! connection (see the per-connection `SkillRegistry` in `main.rs`).
//!
//! A load failure does not abort the daemon; we surface it as an empty
//! catalog + a `tracing::warn!`, so a corrupt or absent skills dir doesn't
//! deny service. Re-loading at runtime is a P-future polish item.

use origin_skills::{load_all, LoaderError, Skill};
use std::path::Path;
use std::sync::Arc;

/// Read-only catalog of every `SKILL.md` available to the daemon.
///
/// Always includes the 14 embedded superpowers skills; entries under `root`
/// (defaults to `~/.origin/skills/`) override embedded ones with the same name.
#[derive(Debug, Default)]
pub struct SkillCatalog {
    skills: Vec<Skill>,
}

impl SkillCatalog {
    /// Load embedded skills plus any user overrides under `root`. The 14
    /// superpowers skills are always present even if `root` does not exist.
    ///
    /// # Errors
    ///
    /// Returns a [`LoaderError`] if a skill directory exists at `root` but
    /// cannot be walked, or if any individual skill's `SKILL.md` frontmatter
    /// fails to parse / validate.
    pub fn load_from(root: &Path) -> Result<Self, LoaderError> {
        let skills = load_all(root)?;
        Ok(Self { skills })
    }

    /// Best-effort variant for the daemon boot path: any error degrades
    /// to an empty catalog with a warning, so a malformed skill can't
    /// keep the daemon from coming up.
    #[must_use]
    pub fn load_or_empty(root: &Path) -> Arc<Self> {
        match Self::load_from(root) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                tracing::warn!(error = %e, path = %root.display(), "skill catalog load failed; running with empty catalog");
                Arc::new(Self::default())
            }
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Find a skill by its frontmatter `name`. `None` when not present.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.front.name == name)
    }

    /// Iterate every skill in catalog order (filesystem walk order, as
    /// returned by `load_skills_dir`).
    pub fn iter(&self) -> impl Iterator<Item = &Skill> {
        self.skills.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(dir: &Path, name: &str, allowed: &[&str]) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).expect("mkdir");
        let allowed = allowed
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let body =
            format!("---\nname: {name}\ndescription: test skill\nallowed-tools: [{allowed}]\n---\nbody\n");
        std::fs::write(skill_dir.join("SKILL.md"), body).expect("write");
    }

    #[test]
    fn load_includes_embedded_when_user_dir_missing() {
        // load_all always returns the 14 embedded superpowers skills, even if
        // the user override dir does not exist.
        let dir = tempfile::tempdir().expect("tempdir");
        let cat = SkillCatalog::load_from(&dir.path().join("nope")).expect("ok");
        assert!(cat.find("brainstorming").is_some(), "embedded skills must appear");
        assert!(
            cat.len() >= 14,
            "expected >=14 embedded skills, got {}",
            cat.len()
        );
    }

    #[test]
    fn user_skills_merge_with_embedded() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "foo", &["Read"]);
        write_skill(dir.path(), "bar", &["Glob", "Grep"]);
        let cat = SkillCatalog::load_from(dir.path()).expect("ok");
        assert!(cat.find("foo").is_some());
        assert!(cat.find("bar").is_some());
        assert!(
            cat.find("brainstorming").is_some(),
            "embedded skill still present"
        );
        assert!(cat.find("missing").is_none());
        assert!(
            cat.len() >= 16,
            "expected >=16 (14 embedded + 2 user), got {}",
            cat.len()
        );
    }

    #[test]
    fn load_or_empty_degrades_on_corrupt_frontmatter() {
        // Contract: `load_skills_dir` is fail-fast. A SINGLE corrupt SKILL.md
        // causes `load_or_empty` to surface every skill in the directory as
        // empty — including valid ones alongside the bad file. This is the
        // intended degradation: we'd rather show no skills than half a catalog
        // with no signal about the broken file.
        let dir = tempfile::tempdir().expect("tempdir");
        // A valid skill that would normally load fine.
        write_skill(dir.path(), "good", &["Read"]);
        // A sibling skill with no frontmatter — this poisons the entire load.
        let skill_dir = dir.path().join("broken");
        std::fs::create_dir_all(&skill_dir).expect("mkdir");
        std::fs::write(skill_dir.join("SKILL.md"), "no frontmatter here").expect("write");
        let cat = SkillCatalog::load_or_empty(dir.path());
        assert!(
            cat.is_empty(),
            "corrupt frontmatter must drop the entire catalog, including the valid sibling"
        );
    }
}
