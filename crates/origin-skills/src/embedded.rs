// SPDX-License-Identifier: Apache-2.0
//! Embedded superpowers skill bodies. Walked at runtime by `load_embedded`.
//!
//! Built-time inclusion means the binary ships with every skill; users do not
//! need to copy files anywhere to use them.

use crate::frontmatter::{parse_frontmatter, ParsedSkill};
use crate::loader::{Skill, SkillHash};

static EMBEDDED: include_dir::Dir<'_> = include_dir::include_dir!("$CARGO_MANIFEST_DIR/embedded/superpowers");

/// Walk the embedded `superpowers/` tree and return every `SKILL.md` parsed
/// into a [`Skill`].
///
/// Malformed frontmatter is treated as a build-time bug — we panic in that
/// case because the skills are vendored, not user input.
///
/// # Panics
///
/// Panics if any vendored `SKILL.md` is not valid UTF-8 or fails frontmatter
/// parsing. Both conditions are caught at build time by the embed step in
/// CI; reaching them at runtime means the binary was assembled incorrectly.
// name disambiguates from `load_all` and `load_skills_dir` at call sites.
#[allow(clippy::module_name_repetitions)]
#[must_use]
pub fn load_embedded() -> Vec<Skill> {
    let mut out = Vec::new();
    for dir in EMBEDDED.dirs() {
        let Some(file) = dir.get_file(format!("{}/SKILL.md", dir.path().display())) else {
            continue;
        };
        let raw = std::str::from_utf8(file.contents()).expect("embedded SKILL.md is utf-8");
        let ParsedSkill { front, body } =
            parse_frontmatter(raw).expect("embedded SKILL.md frontmatter is valid");
        let body_hash = SkillHash(*blake3::hash(body.as_bytes()).as_bytes());
        out.push(Skill {
            front,
            body,
            body_hash,
            source: file.path().to_path_buf(),
        });
    }
    out
}
